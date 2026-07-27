import assert from "node:assert/strict";
import test from "node:test";
import { DecodeStallEvidence } from "../src/video-decoder.ts";

test("transport packets without decode submissions are not stall evidence", () => {
  const evidence = new DecodeStallEvidence();

  assert.deepEqual(evidence.sample(0), { submitted: 0, silentSubmissions: 0 });
  assert.deepEqual(evidence.sample(0), { submitted: 0, silentSubmissions: 0 });
});

test("silent evidence counts successful submissions across samples", () => {
  const evidence = new DecodeStallEvidence();
  for (let i = 0; i < 19; i += 1) evidence.noteSubmission();
  assert.deepEqual(evidence.sample(0), { submitted: 19, silentSubmissions: 19 });

  evidence.noteSubmission();
  assert.deepEqual(evidence.sample(0), { submitted: 1, silentSubmissions: 20 });
});

test("decoder output and rung reset clear accumulated evidence", () => {
  const evidence = new DecodeStallEvidence();
  evidence.noteSubmission();
  evidence.noteSubmission();
  assert.equal(evidence.sample(0).silentSubmissions, 2);

  assert.equal(evidence.sample(1).silentSubmissions, 0);
  evidence.noteSubmission();
  evidence.reset();
  assert.deepEqual(evidence.sample(0), { submitted: 0, silentSubmissions: 0 });
});
