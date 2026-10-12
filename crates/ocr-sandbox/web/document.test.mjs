import test from 'node:test';
import assert from 'node:assert/strict';
import {prepareFile} from './document.mjs';

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
