#!/usr/bin/env node
// vrt-compare.mjs — pixel-level comparison of two PNG screenshots (gui-vrt skill).
//
// Usage:
//   node scripts/vrt-compare.mjs <expected.png> <actual.png> <diff.png> [--threshold 0.1]
//
// Prints ONE JSON object on stdout:
//   {"similarity_pct":0.9987,"mismatched_pixels":12,"total_pixels":9000,
//    "width":100,"height":90,"threshold":0.1,"diff_path":"diff.png","size_mismatch":false}
//
// similarity_pct is a RATIO in 0.0..1.0 (1.0 = identical), despite the name — this is
// the unit SPEC-GUI-001 specifies for the `diff` evidence entry.
// `threshold` is pixelmatch's per-pixel colour tolerance (0..1), not a pass/fail limit;
// the pass/fail limit (`min_similarity`) is applied by the caller.
//
// Exit codes: 0 = compared (images may still differ), 2 = usage / IO / decode error.
// Images with different dimensions are NOT an exception: similarity_pct is 0, size_mismatch
// is true, and no diff image is written (there is nothing meaningful to overlay).
import { readFileSync, writeFileSync } from 'node:fs';
import { PNG } from 'pngjs';
import pixelmatch from 'pixelmatch';

const DEFAULT_THRESHOLD = 0.1;

function fail(message) {
  process.stderr.write(`vrt-compare: ${message}\n`);
  process.exit(2);
}

function parseArgs(argv) {
  const positional = [];
  let threshold = DEFAULT_THRESHOLD;
  for (let i = 0; i < argv.length; i += 1) {
    if (argv[i] === '--threshold') {
      threshold = Number(argv[i + 1]);
      i += 1;
      if (!Number.isFinite(threshold) || threshold < 0 || threshold > 1) {
        fail('--threshold must be a number between 0 and 1');
      }
    } else {
      positional.push(argv[i]);
    }
  }
  if (positional.length !== 3) {
    fail('usage: vrt-compare.mjs <expected.png> <actual.png> <diff.png> [--threshold 0.1]');
  }
  return { expectedPath: positional[0], actualPath: positional[1], diffPath: positional[2], threshold };
}

function readPng(path) {
  try {
    return PNG.sync.read(readFileSync(path));
  } catch (error) {
    return fail(`cannot read PNG ${path}: ${error.message}`);
  }
}

const { expectedPath, actualPath, diffPath, threshold } = parseArgs(process.argv.slice(2));
const expected = readPng(expectedPath);
const actual = readPng(actualPath);

let result;
if (expected.width !== actual.width || expected.height !== actual.height) {
  result = {
    similarity_pct: 0,
    mismatched_pixels: null,
    total_pixels: null,
    width: null,
    height: null,
    threshold,
    diff_path: null,
    size_mismatch: true,
    expected_size: `${expected.width}x${expected.height}`,
    actual_size: `${actual.width}x${actual.height}`,
  };
} else {
  const { width, height } = expected;
  const diff = new PNG({ width, height });
  const mismatched = pixelmatch(expected.data, actual.data, diff.data, width, height, { threshold });
  try {
    writeFileSync(diffPath, PNG.sync.write(diff));
  } catch (error) {
    fail(`cannot write diff image ${diffPath}: ${error.message}`);
  }
  const total = width * height;
  result = {
    similarity_pct: total === 0 ? 1 : Number((1 - mismatched / total).toFixed(6)),
    mismatched_pixels: mismatched,
    total_pixels: total,
    width,
    height,
    threshold,
    diff_path: diffPath,
    size_mismatch: false,
  };
}
process.stdout.write(`${JSON.stringify(result)}\n`);
