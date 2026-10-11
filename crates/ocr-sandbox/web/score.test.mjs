import test from 'node:test';
import assert from 'node:assert/strict';
import { score } from './score.mjs';

test('user uploads receive no invented benchmark score', () => {
  assert.equal(score(null, {text:'hello', fields:{}}), null);
});
test('reference fields are compared with extracted values', () => {
  const sample={expected_fields:{sodium:200,serving:{amount:50,unit:'g'}},reference_text:null};
  assert.deepEqual(score(sample,{fields:{sodium:{value:200},serving:{value:{unit:'g',amount:50}}}}),{kind:'fields',correct:2,total:2});
  assert.equal(score(sample,{fields:{sodium:{value:400}}}).correct,0);
});
test('blank samples must remain blank', () => {
  assert.deepEqual(score({reference_text:'',expected_fields:{}},{text:'imagined food'}),{kind:'blank',passed:false});
});
