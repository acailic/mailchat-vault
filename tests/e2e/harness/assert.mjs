// Minimal assertion helpers for the E2E specs: plain throws with readable
// messages, no test-framework dependency (the runner reports them).

export function ok(condition, message) {
  if (!condition) {
    throw new Error(`assert ok failed: ${message}`);
  }
}

export function equal(actual, expected, message) {
  if (actual !== expected) {
    throw new Error(
      `assert equal failed: ${message}\n  expected: ${JSON.stringify(expected)}\n  actual:   ${JSON.stringify(actual)}`,
    );
  }
}

export function notEqual(actual, expected, message) {
  if (actual === expected) {
    throw new Error(
      `assert notEqual failed: ${message}\n  both were: ${JSON.stringify(actual)}`,
    );
  }
}

export function includes(haystack, needle, message) {
  if (typeof haystack !== "string" || !haystack.includes(needle)) {
    throw new Error(
      `assert includes failed: ${message}\n  needle:   ${JSON.stringify(needle)}\n  haystack: ${JSON.stringify(haystack)}`,
    );
  }
}

export function matches(haystack, pattern, message) {
  if (typeof haystack !== "string" || !pattern.test(haystack)) {
    throw new Error(
      `assert matches failed: ${message}\n  pattern:  ${pattern}\n  haystack: ${JSON.stringify(haystack)}`,
    );
  }
}

export function deepEqual(actual, expected, message) {
  const norm = (value) => JSON.stringify(value, Object.keys(value ?? {}).sort());
  const a = norm(actual);
  const e = norm(expected);
  if (a !== e) {
    throw new Error(
      `assert deepEqual failed: ${message}\n  expected: ${e}\n  actual:   ${a}`,
    );
  }
}
