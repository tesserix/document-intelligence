import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';

test('native file picker includes PDF, PNG and both JPEG extensions', async () => {
  const html = await readFile(new URL('./index.html', import.meta.url), 'utf8');
  const accept = html.match(/id="file"[^>]*accept="([^"]+)"/)[1].split(',');
  for (const extension of ['.pdf', '.png', '.jpg', '.jpeg']) assert.ok(accept.includes(extension), extension);
});
