import assert from "node:assert/strict";
import test from "node:test";
import { handleUnhandledRejection } from "../src/promise-rejection.ts";

test("production promise rejection is recorded without replacing the UI", () => {
  const reason = new Error("no joined network to reconnect on");
  let prevented = false;
  const fatal: unknown[] = [];
  const reports: Array<[string, unknown]> = [];

  handleUnhandledRejection(
    {
      reason,
      preventDefault: () => {
        prevented = true;
      },
    },
    false,
    (label, value) => fatal.push([label, value]),
    (label, value) => reports.push([label, value]),
  );

  assert.equal(prevented, true);
  assert.deepEqual(fatal, []);
  assert.deepEqual(reports, [["Unhandled promise rejection", reason]]);
});

test("development promise rejection keeps the visible diagnostic", () => {
  const fatal: Array<[string, unknown]> = [];
  handleUnhandledRejection(
    {
      reason: "boom",
      preventDefault: () => {},
    },
    true,
    (label, value) => fatal.push([label, value]),
    () => {},
  );

  assert.deepEqual(fatal, [["Unhandled promise rejection", "boom"]]);
});
