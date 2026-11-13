import { expect, authedTest as test } from './fixtures';

interface Namespace {
  name: string;
  security_level: string;
}

test('changes the security level of a namespace', async ({ page, api, namespace }) => {
  const level = async () =>
    (await api.get<Namespace[]>('/namespaces')).find((n) => n.name === namespace)?.security_level;
  expect(await level()).toBe('baseline');

  await page.goto('/namespaces');
  const row = page.getByRole('row', { name: new RegExp(namespace) });
  await row
    .getByRole('combobox', { name: `Change the security level of ${namespace}` })
    .selectOption('privileged');
  await row.getByRole('button', { name: 'Make privileged' }).click();

  await expect.poll(level).toBe('privileged');
  await expect(row.getByRole('cell', { name: 'privileged', exact: true })).toBeVisible();
});
