import test from 'node:test';
import assert from 'node:assert/strict';
import {isTerminalJob, isFailedJob} from './status.mjs';
test('rejected and cancelled jobs stop polling without retrieving a result',()=>{
  for(const state of ['rejected','cancelled','failed']){assert.ok(isTerminalJob(state));assert.ok(isFailedJob(state));}
  for(const state of ['completed','partial','review_required']){assert.ok(isTerminalJob(state));assert.equal(isFailedJob(state),false);}
  for(const state of ['accepted','inspecting','processing','validating','cancelling']){assert.equal(isTerminalJob(state),false);}
});
