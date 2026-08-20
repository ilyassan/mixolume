import "@testing-library/jest-dom/vitest";
import { afterEach } from "vitest";
import { cleanup } from "@testing-library/react";

// Without this, a component/hook rendered in one test (and any listeners or
// timers it registers) stays mounted into the next test and causes
// cross-test leakage.
afterEach(() => {
  cleanup();
});
