import { expect, authedTest as test } from './fixtures';

test('creates a secret, refuses to delete it while used, then deletes it anyway', async ({
  page,
  api,
  namespace
}) => {
  await page.goto('/secrets');
  await page.getByRole('button', { name: 'New secret' }).click();
  await page.getByLabel('Namespace').fill(namespace);
  await page.getByLabel('Name', { exact: true }).fill('db-pass');
  await page.getByLabel('Value').fill('s3cr3t');
  await page.getByRole('button', { name: 'Create' }).click();

  const row = page.getByRole('row', { name: new RegExp(`db-pass.*${namespace}`) });
  await expect(row).toBeVisible();

  // A deployment that uses it makes the API refuse a plain delete.
  await api.post('/deployments', {
    namespace,
    name: 'uses-secret',
    image: 'busybox:latest',
    runtime: 'docker',
    command: ['sleep', '3600'],
    environment: { DB_PASS: { secretRef: 'db-pass' } }
  });

  await row.getByRole('button', { name: 'Delete' }).click();
  await row.getByRole('button', { name: 'Delete' }).click();
  await expect(row.getByText(`Used by ${namespace}/uses-secret`)).toBeVisible();

  await row.getByRole('button', { name: 'Delete anyway' }).click();
  await row.getByRole('button', { name: 'Delete anyway' }).click();
  await expect(row).toBeHidden();

  const secrets = await api.get<Array<{ namespace: string }>>('/secrets');
  expect(secrets.filter((s) => s.namespace === namespace)).toEqual([]);
});

test('keeps a validation error on the form', async ({ page, namespace }) => {
  await page.goto('/secrets');
  await page.getByRole('button', { name: 'New secret' }).click();
  await page.getByLabel('Namespace').fill(namespace);
  await page.getByLabel('Name', { exact: true }).fill('x');
  await page.getByLabel('Value').fill('v');
  await page.getByRole('button', { name: 'Create' }).click();
  await expect(page.getByText('name: must be 2 to 253 characters')).toBeVisible();
  await expect(page.getByRole('heading', { name: 'New secret' })).toBeVisible();
});
