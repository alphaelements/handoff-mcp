// Tests for scripts/build-npm-packages.js. Lives in bin/ so the single
// `node --test "bin/*.test.js"` glob covers the whole npm-distribution layer.

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import { TARGETS, manifestFor, binaryName } from "../scripts/build-npm-packages.js";
import { binaryEntry, platformPackage, SUPPORTED } from "./resolve-binary.js";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const rootPkg = JSON.parse(fs.readFileSync(path.join(ROOT, "package.json"), "utf8"));

// ============================================================
// The three lists that must agree: build targets, wrapper
// resolution table, and the wrapper's optionalDependencies.
// ============================================================
test("every Rust target produces a package the wrapper knows how to resolve", () => {
  for (const target of Object.keys(TARGETS)) {
    const m = manifestFor(target, rootPkg.version, rootPkg);
    assert.equal(
      m.name,
      platformPackage(m.os[0], m.cpu[0]),
      `${target} builds "${m.name}" but the wrapper resolves a different name`
    );
  }
});

test("the build matrix and the wrapper resolution table are the same size", () => {
  assert.equal(Object.keys(TARGETS).length, Object.keys(SUPPORTED).length);
});

test("the release build matrix covers exactly the targets we package", () => {
  // The fourth list. Without this, release.yml could drop or rename a target
  // and every other gate would still pass — the failure would first appear as
  // a published optionalDependency with no binary behind it.
  const yml = fs.readFileSync(
    path.join(ROOT, ".github", "workflows", "release.yml"),
    "utf8"
  );
  // Deliberately a regex rather than a YAML parser: the test must not need a
  // dependency, and `- target: <triple>` is the only shape this file uses.
  const matrix = [...yml.matchAll(/^\s*-\s*target:\s*(\S+)\s*$/gm)].map((m) => m[1]);
  assert.ok(matrix.length > 0, "no build matrix targets found in release.yml");
  assert.deepEqual(
    [...new Set(matrix)].sort(),
    Object.keys(TARGETS).sort(),
    "release.yml's build matrix and build-npm-packages.js TARGETS disagree"
  );
});

test("every platform package is declared as an optionalDependency of the wrapper", () => {
  const declared = Object.keys(rootPkg.optionalDependencies || {}).sort();
  const built = Object.keys(TARGETS)
    .map((t) => manifestFor(t, rootPkg.version, rootPkg).name)
    .sort();
  assert.deepEqual(declared, built);
});

test("optionalDependencies are pinned exactly to the wrapper version", () => {
  for (const [name, range] of Object.entries(rootPkg.optionalDependencies || {})) {
    assert.equal(
      range,
      rootPkg.version,
      `${name} must be pinned to ${rootPkg.version}, not a range like "${range}"`
    );
    assert.doesNotMatch(range, /[\^~*x]|\s-\s|>=|<=/, `${name} must not use a range operator`);
  }
});

// ============================================================
// Manifest contents
// ============================================================
test("os and cpu are single-valued so npm can skip non-matching packages", () => {
  for (const target of Object.keys(TARGETS)) {
    const m = manifestFor(target, "1.2.3", rootPkg);
    assert.equal(m.os.length, 1);
    assert.equal(m.cpu.length, 1);
    assert.equal(m.version, "1.2.3");
  }
});

test("platform packages declare no bin — the wrapper owns the CLI name", () => {
  for (const target of Object.keys(TARGETS)) {
    const m = manifestFor(target, "1.2.3", rootPkg);
    assert.equal(m.bin, undefined, `${target} must not declare a bin entry`);
  }
});

test("preferUnplugged is set so Yarn PnP cannot leave the binary zipped", () => {
  for (const target of Object.keys(TARGETS)) {
    assert.equal(manifestFor(target, "1.2.3", rootPkg).preferUnplugged, true);
  }
});

test("the two Linux packages declare glibc; the others declare no libc", () => {
  assert.deepEqual(manifestFor("x86_64-unknown-linux-gnu", "1.2.3", rootPkg).libc, ["glibc"]);
  assert.deepEqual(manifestFor("aarch64-unknown-linux-gnu", "1.2.3", rootPkg).libc, ["glibc"]);
  assert.equal(manifestFor("aarch64-apple-darwin", "1.2.3", rootPkg).libc, undefined);
  assert.equal(manifestFor("x86_64-pc-windows-msvc", "1.2.3", rootPkg).libc, undefined);
});

test("only the bin directory and license files are published — no source, no build files", () => {
  for (const target of Object.keys(TARGETS)) {
    assert.deepEqual(manifestFor(target, "1.2.3", rootPkg).files, ["bin/", "THIRD_PARTY_LICENSES.md"]);
  }
});

// ============================================================
// Binary naming
// ============================================================
test("Windows targets carry a .exe; the others do not", () => {
  assert.equal(binaryName("x86_64-pc-windows-msvc"), "handoff-mcp.exe");
  assert.equal(binaryName("aarch64-pc-windows-msvc"), "handoff-mcp.exe");
  assert.equal(binaryName("x86_64-unknown-linux-gnu"), "handoff-mcp");
  assert.equal(binaryName("aarch64-apple-darwin"), "handoff-mcp");
});

test("the built binary name matches what the wrapper resolves inside the package", () => {
  for (const target of Object.keys(TARGETS)) {
    const m = manifestFor(target, "1.2.3", rootPkg);
    assert.equal(binaryName(target), binaryEntry(m.os[0]));
  }
});

// ============================================================
// The wrapper package itself
// ============================================================
test("the wrapper runs no install scripts — npm v12 disables them by default", () => {
  const scripts = rootPkg.scripts || {};
  for (const hook of ["preinstall", "install", "postinstall", "prepare"]) {
    assert.equal(scripts[hook], undefined, `"${hook}" would not run under npm v12`);
  }
});

test("the published wrapper ships both wrapper files and no Rust sources", () => {
  const files = rootPkg.files;
  assert.ok(files.includes("bin/handoff-mcp.js"));
  assert.ok(files.includes("bin/resolve-binary.js"), "the wrapper requires this at runtime");
  assert.ok(!files.includes("src/"), "Rust sources are no longer needed to install");
  assert.ok(!files.includes("Cargo.toml"), "nothing is built at install time");
});

test("scripts/postinstall.js is gone, not merely unreferenced", () => {
  assert.ok(
    !fs.existsSync(path.join(ROOT, "scripts", "postinstall.js")),
    "leaving the file invites it being re-wired into package.json"
  );
});

// ============================================================
// Release workflow: post-publish registry propagation checks.
// v0.41.0 took ~6 min for a freshly published platform package to become
// visible; a 3.5 min retry window failed the job and skipped the wrapper.
// ============================================================
const WAIT_SCRIPT = path.join(ROOT, "scripts", "wait-npm-live.sh");
const releaseYml = () =>
  fs.readFileSync(path.join(ROOT, ".github", "workflows", "release.yml"), "utf8");

// Runs wait-npm-live.sh with a fake `curl` on PATH. `liveAfter` = number of
// curl calls that fail before the fake starts succeeding (Infinity = never).
function runWait({ args, liveAfter, timeout = "3", interval = "1" }) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "wait-npm-"));
  const counter = path.join(dir, "count");
  fs.writeFileSync(counter, "0");
  const fakeCurl = path.join(dir, "curl");
  fs.writeFileSync(
    fakeCurl,
    `#!/bin/sh
echo "$@" >> "${dir}/argv"
n=$(cat "${counter}"); n=$((n+1)); echo $n > "${counter}"
[ "$n" -gt ${Number.isFinite(liveAfter) ? liveAfter : 1000000} ]
`,
    { mode: 0o755 }
  );
  const r = spawnSync("bash", [WAIT_SCRIPT, ...args], {
    env: {
      ...process.env,
      PATH: `${dir}:${process.env.PATH}`,
      NPM_WAIT_TIMEOUT_SECONDS: timeout,
      NPM_WAIT_INTERVAL_SECONDS: interval,
    },
    encoding: "utf8",
  });
  const argv = fs.existsSync(path.join(dir, "argv"))
    ? fs.readFileSync(path.join(dir, "argv"), "utf8")
    : "";
  fs.rmSync(dir, { recursive: true, force: true });
  return { ...r, argv };
}

test("wait-npm-live succeeds once the package appears and queries the registry API uncached", () => {
  const r = runWait({ args: ["1.2.3", "pkg-a"], liveAfter: 1 });
  assert.equal(r.status, 0, r.stderr + r.stdout);
  assert.match(r.stdout, /ok: pkg-a@1\.2\.3/);
  assert.match(r.argv, /https:\/\/registry\.npmjs\.org\/pkg-a\/1\.2\.3/);
  assert.match(r.argv, /Cache-Control: no-cache/i);
});

test("wait-npm-live fails with an error naming the package when it never appears", () => {
  const r = runWait({ args: ["1.2.3", "pkg-a", "pkg-b"], liveAfter: Infinity, timeout: "2" });
  assert.notEqual(r.status, 0);
  assert.match(r.stdout + r.stderr, /::error::pkg-a@1\.2\.3 is not on the registry/);
  assert.match(r.stdout + r.stderr, /::error::pkg-b@1\.2\.3 is not on the registry/);
});

test("wait-npm-live requires a version and at least one package", () => {
  assert.notEqual(runWait({ args: [], liveAfter: 0 }).status, 0);
  assert.notEqual(runWait({ args: ["1.2.3"], liveAfter: 0 }).status, 0);
});

test("release.yml waits long enough (10-20 min) for npm propagation, for platform packages and the wrapper", () => {
  const yml = releaseYml();
  const m = yml.match(/NPM_WAIT_TIMEOUT_SECONDS:\s*"?(\d+)"?/g) || [];
  assert.ok(m.length >= 2, "both the platform-package and wrapper checks must set a timeout");
  for (const line of m) {
    const secs = Number(line.match(/(\d+)/)[1]);
    assert.ok(secs >= 600 && secs <= 1200, `timeout ${secs}s outside 600..1200`);
  }
  // A call is the command line plus any backslash-continued lines.
  const calls = [...yml.matchAll(/wait-npm-live\.sh "\$version"(?:[^\n]*\\\n)*[^\n]*/g)];
  const waited = calls.flatMap((c) => c[0].match(/handoff-mcp-server[\w-]*/g));
  const expected = [
    ...Object.keys(TARGETS).map((t) => manifestFor(t, rootPkg.version, rootPkg).name),
    "handoff-mcp-server",
  ].sort();
  assert.deepEqual([...new Set(waited)].sort(), expected);
});
