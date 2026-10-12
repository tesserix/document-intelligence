import test from 'node:test';
import assert from 'node:assert/strict';

test('browser stops polling rejected jobs and restores upload controls', async () => {
  const elements = new Map();
  const element = id => {
    if (!elements.has(id)) elements.set(id, {
      listeners: {}, hidden: false, disabled: false, checked: false,
      classList: {toggle() {}},
      addEventListener(name, callback) { this.listeners[name] = callback; },
    });
    return elements.get(id);
  };
  const original = {document: globalThis.document, location: globalThis.location, fetch: globalThis.fetch};
  globalThis.document = {getElementById: element, querySelectorAll: () => []};
  globalThis.location = {origin: 'https://sandbox.example'};
  let polls = 0;
  let results = 0;
  globalThis.fetch = async (path, options) => {
    let body = {};
    if (path === '/v1/sandbox/sessions') body = {token: 'synthetic'};
    else if (path === '/v1/ocr/uploads') body = {upload_id: 'fixture', upload_url: 'https://sandbox.example/content'};
    else if (path === '/v1/ocr/uploads/fixture') body = {status: 'accepted'};
    else if (path === '/v1/ocr/jobs' && options.method === 'POST') body = {job_id: 'fixture'};
    else if (path === '/v1/ocr/jobs/fixture') {
      polls++;
      if (polls > 1) throw new Error('Unexpected poll after rejection');
      body = {status: 'rejected'};
    } else if (path.endsWith('/result')) results++;
    return new Response(JSON.stringify(body), {status: 200});
  };
  try {
    await import('./app.mjs');
    const file = new File([new Uint8Array([137,80,78,71,13,10,26,10])], 'fixture.png', {type: 'image/png'});
    await element('file').listeners.change({target: {files: [file]}});
    await element('run').listeners.click();
    assert.equal(polls, 1);
    assert.equal(results, 0);
    assert.match(element('error').textContent, /OCR could not complete/);
    assert.equal(element('inputs').disabled, false);
    assert.equal(element('run').disabled, false);
  } finally {
    Object.assign(globalThis, original);
  }
});
