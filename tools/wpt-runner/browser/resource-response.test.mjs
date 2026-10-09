import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { resourceResponse } from './resource-response.mjs';

async function fixture(t, extension, sidecar) {
  const dir = await mkdtemp(join(tmpdir(), 'w3cos-wpt-response-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const path = join(dir, `resource.${extension}`);
  await writeFile(path, 'div { background: red }');
  if (sidecar !== undefined) await writeFile(`${path}.headers`, sidecar);
  return path;
}

test('ordinary CSS uses its default media type', async t => {
  const response = await resourceResponse(await fixture(t, 'css'));
  assert.equal(response.headers['content-type'], 'text/css');
  assert.equal(response.protocol.sidecar_sha256, null);
});

test('WPT sidecar overrides CSS extension without changing response bytes', async t => {
  const path = await fixture(t, 'css', 'Content-Type: text/plain\n');
  const response = await resourceResponse(path);
  assert.equal(response.headers['content-type'], 'text/plain');
  assert.equal(response.protocol.content_type, 'text/plain');
  assert.match(response.protocol.sidecar_sha256, /^[a-f0-9]{64}$/);
  assert.equal(response.body.toString(), 'div { background: red }');
});

test('case insensitive headers retain declared charset and extra fields', async t => {
  const response = await resourceResponse(await fixture(t, 'css',
    '# comment\ncontent-TYPE: text/css; charset=Shift_JIS\nX-Test: sidecar\n'));
  assert.equal(response.headers['content-type'], 'text/css; charset=Shift_JIS');
  assert.equal(response.headers['x-test'], 'sidecar');
});

test('invalid header lines follow native server validation', async t => {
  const response = await resourceResponse(await fixture(t, 'css',
    'not a header\nBad Header: ignored\n: ignored\nX-Valid: kept\nContent-Length: 1\nConnection: keep-alive\n'));
  assert.equal(response.headers['content-type'], 'text/css');
  assert.deepEqual(Object.keys(response.headers).sort(), ['content-type', 'x-valid']);
});
