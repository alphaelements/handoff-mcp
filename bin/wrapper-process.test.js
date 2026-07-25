"use strict";

// Process-level tests for bin/handoff-mcp.js. These spawn the real wrapper
// against a real binary — the resolution unit tests in resolve-binary.test.js
// cannot catch process-lifecycle defects (orphaned children, swallowed exit
// codes), because those only exist once there is an actual child process.
//
// The wrapper is pointed at a stand-in binary via HANDOFF_MCP_BINARY_PATH, so
// these run without installing anything.

const { test } = require("node:test");
const assert = require("node:assert/strict");
const { spawn } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const WRAPPER = path.join(__dirname, "handoff-mcp.js");

// A stand-in for the Rust binary: sleeps until signalled, so the test can
// observe what happens to it when the *wrapper* is signalled.
function makeLongRunningStub(dir) {
  const stub = path.join(dir, "stub.js");
  fs.writeFileSync(
    stub,
    [
      "process.stdout.write('ready\\n');",
      // Keep the process alive indefinitely; default SIGTERM handling applies.
      "setInterval(() => {}, 1000);",
    ].join("\n")
  );
  return stub;
}

function makeExitingStub(dir, code) {
  const stub = path.join(dir, `exit${code}.js`);
  fs.writeFileSync(stub, `process.exit(${code});`);
  return stub;
}

function spawnWrapper(binaryPath, args = [], opts = {}) {
  return spawn(process.execPath, [WRAPPER, ...args], {
    env: { ...process.env, HANDOFF_MCP_BINARY_PATH: `${process.execPath} ${binaryPath}` },
    ...opts,
  });
}

// The override takes a single path, so wrap the stub in a tiny executable
// shim rather than trying to smuggle an argv through the env var.
function makeExecutableStub(dir, jsPath) {
  if (process.platform === "win32") return null; // shell shim is POSIX-only
  const sh = path.join(dir, `${path.basename(jsPath, ".js")}.sh`);
  fs.writeFileSync(sh, `#!/bin/sh\nexec ${process.execPath} ${jsPath} "$@"\n`);
  fs.chmodSync(sh, 0o755);
  return sh;
}

const canRun = process.platform !== "win32";

test(
  "signalling the wrapper terminates the child instead of orphaning it",
  { skip: canRun ? false : "POSIX signal semantics only" },
  async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "handoff-wrap-"));
    const stub = makeExecutableStub(dir, makeLongRunningStub(dir));

    const wrapper = spawn(process.execPath, [WRAPPER], {
      env: { ...process.env, HANDOFF_MCP_BINARY_PATH: stub },
      stdio: ["pipe", "pipe", "ignore"],
    });

    // Wait for the stub to announce itself, so the child definitely exists.
    await new Promise((resolve, reject) => {
      let seen = "";
      wrapper.stdout.on("data", (d) => {
        seen += d.toString();
        if (seen.includes("ready")) resolve();
      });
      wrapper.on("error", reject);
      setTimeout(() => reject(new Error("stub never became ready")), 10000);
    });

    // The child is a grandchild of this test process; find it by parent pid.
    const { execFileSync } = require("node:child_process");
    const childPid = execFileSync("pgrep", ["-P", String(wrapper.pid)])
      .toString()
      .trim()
      .split("\n")
      .filter(Boolean)
      .map(Number)[0];
    assert.ok(childPid, "expected the wrapper to have spawned a child");

    const closed = new Promise((resolve) =>
      wrapper.on("close", (code, signal) => resolve({ code, signal }))
    );
    wrapper.kill("SIGTERM");
    const outcome = await closed;

    // Give the OS a moment to reap the child before checking.
    await new Promise((r) => setTimeout(r, 300));

    let childAlive = true;
    try {
      process.kill(childPid, 0);
    } catch {
      childAlive = false;
    }

    assert.equal(
      childAlive,
      false,
      "the child outlived the wrapper — it was orphaned to init, leaking a " +
        "server process that still holds .handoff/ file handles"
    );

    // The wrapper must die *from* the signal, so callers see the same thing
    // they would if they had signalled the binary directly.
    assert.equal(outcome.signal, "SIGTERM");
    assert.equal(outcome.code, null);

    fs.rmSync(dir, { recursive: true, force: true });
  }
);

test(
  "the child's exit code passes through the wrapper unchanged",
  { skip: canRun ? false : "POSIX shim only" },
  async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "handoff-wrap-"));
    for (const code of [0, 1, 42]) {
      const stub = makeExecutableStub(dir, makeExitingStub(dir, code));
      const wrapper = spawn(process.execPath, [WRAPPER], {
        env: { ...process.env, HANDOFF_MCP_BINARY_PATH: stub },
        stdio: "ignore",
      });
      const actual = await new Promise((resolve) => wrapper.on("close", resolve));
      assert.equal(actual, code, `exit code ${code} was not propagated`);
    }
    fs.rmSync(dir, { recursive: true, force: true });
  }
);

test("an unresolvable binary exits 1 with a message naming the override", async () => {
  const wrapper = spawn(process.execPath, [WRAPPER], {
    env: { ...process.env, HANDOFF_MCP_BINARY_PATH: "/nonexistent/handoff-mcp" },
    stdio: ["ignore", "ignore", "pipe"],
  });
  let stderr = "";
  wrapper.stderr.on("data", (d) => (stderr += d.toString()));
  const code = await new Promise((resolve) => wrapper.on("close", resolve));
  assert.equal(code, 1);
  assert.match(stderr, /HANDOFF_MCP_BINARY_PATH/);
  assert.match(stderr, /\/nonexistent\/handoff-mcp/);
});
