import test from 'node:test';
import assert from 'node:assert/strict';
import {prepareFile as prepareWithLimits} from './document.mjs';
const prepareFile = file => prepareWithLimits(file, 20 * 1024 * 1024);

const png = new Uint8Array([137,80,78,71,13,10,26,10,0]);
const jpeg = new Uint8Array([255,216,255,224,0]);
const pdf = '%PDF-1.7\nfixture';

for (const [name,bytes,type] of [['sample.png',png,'image/png'],['sample.jpg',jpeg,'image/jpeg'],['sample.jpeg',jpeg,'image/jpeg'],['sample.pdf',pdf,'application/pdf']]) {
  test(`selects ${name} when the operating system supplies no MIME type`, async () => {
    const file = await prepareFile(new File([bytes],name));
    assert.equal(file.type,type);
    assert.equal(file.name,name);
    assert.equal(file.size,new Blob([bytes]).size);
  });
}

test('rejects spoofed MIME types, unsupported content, empty and oversized files', async () => {
  for (const file of [new File([pdf],'fake.png',{type:'image/png'}),new File(['<svg/>'],'fake.png'),new File([],'empty.pdf'),new File([new Uint8Array(4194305)],'large.png')]) {
    await assert.rejects(prepareFile(file),/PDF, PNG or JPEG/);
  }
});

for (const [name, bytes, type] of [
  ['little.tiff', new Uint8Array([73,73,42,0,8,0,0,0]), 'image/tiff'],
  ['big.tif', new Uint8Array([77,77,0,42,0,0,0,8]), 'image/tiff'],
  ['sample.webp', new Uint8Array([82,73,70,70,4,0,0,0,87,69,66,80]), 'image/webp'],
]) {
  test(`selects ${name} by its signature`, async () => {
    const file = await prepareFile(new File([bytes], name));
    assert.equal(file.type, type);
  });
}

test('accepts a document at 20 MiB and rejects one byte more', async () => {
  const bytes = new Uint8Array(20 * 1024 * 1024);
  bytes.set(png);
  const file = await prepareFile(new File([bytes], 'large.png'));
  assert.equal(file.size, 20 * 1024 * 1024);
  await assert.rejects(prepareFile(new File([bytes, new Uint8Array(1)], 'too-large.png')), /20 MiB/);
});

test('does not admit files without the server upload limit', async () => {
  await assert.rejects(prepareWithLimits(new File([png],'fixture.png'),undefined), /Upload limits are unavailable/);
});
