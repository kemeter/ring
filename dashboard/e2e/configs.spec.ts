import { expect, authedTest as test } from './fixtures';

interface Config {
  id: string;
  namespace: string;
  name: string;
  data: string;
}

async function configData(api: { get<T>(path: string): Promise<T> }, namespace: string) {
  const configs = await api.get<Config[]>('/configs');
  const config = configs.find((c) => c.namespace === namespace && c.name === 'nginx');
  return config ? JSON.parse(config.data) : undefined;
}

test('creates a config file by file, then edits one', async ({ page, api, namespace }) => {
  const siteConf = 'server {\n  listen 80; "quoted"\n}';

  await page.goto('/configs');
  await page.getByRole('button', { name: 'New config' }).click();
  await page.getByLabel('Namespace').fill(namespace);
  await page.getByLabel('Name', { exact: true }).fill('nginx');
  await page.getByLabel('Key').fill('site.conf');
  await page.getByLabel('Contents').fill(siteConf);
  await page.getByRole('button', { name: 'Add a file' }).click();
  await page.getByLabel('Key').nth(1).fill('index.html');
  await page.getByLabel('Contents').nth(1).fill('<h1>hi</h1>');
  await page.getByRole('button', { name: 'Create' }).click();

  // Each file became one entry of the JSON payload, escaping included.
  await expect
    .poll(() => configData(api, namespace))
    .toEqual({ 'site.conf': siteConf, 'index.html': '<h1>hi</h1>' });

  const row = page.getByRole('row', { name: new RegExp(`nginx.*${namespace}`) });
  await row.getByRole('button', { name: 'Edit' }).click();
  await page.getByLabel('Contents').nth(1).fill('<h1>edited</h1>');
  await page.getByRole('button', { name: 'Save' }).click();

  await expect
    .poll(() => configData(api, namespace))
    .toEqual({ 'site.conf': siteConf, 'index.html': '<h1>edited</h1>' });
  await expect(page.getByText('<h1>edited</h1>')).toBeVisible();
});
