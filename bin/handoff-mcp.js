#!/usr/bin/env node

import { spawn } from "node:child_process";

import { resolveBinary } from "./resolve-binary.js";

// Thin wrapper around the prebuilt binary shipped by the per-platform
// optionalDependencies. Deliberately does no work at install time: npm v12
// disables install scripts by default, so anything that needs to happen must
// happen here, at run time.

const result = resolveBinary();

if (!result.ok) {
  console.error(result.message);
  process.exit(1);
}

// Async spawn, not spawnSync. spawnSync blocks the event loop for the entire
// life of the child, which means no signal handler can ever run: an MCP client
// that stops the server signals the wrapper, the wrapper dies instantly, and
// the real server is reparented to init — a leaked process per session, still
// holding .handoff/ file handles. Async spawn keeps the wrapper alive to
// forward the signal.
//
// stdio "inherit" is required, not incidental: this is an MCP stdio server, so
// the child must own the real stdin/stdout and the wrapper must not sit in the
// middle of the JSON-RPC stream.
const child = spawn(result.binary, process.argv.slice(2), { stdio: "inherit" });

// Forwarded rather than handled: the binary owns shutdown (flushing state to
// .handoff/), so the wrapper's only job is to relay the request and wait.
// Registering a handler also stops Node from applying its default terminate
// behavior, which is what kept the wrapper from dying before the child.
const FORWARDED = ["SIGINT", "SIGTERM", "SIGHUP", "SIGQUIT"];
for (const sig of FORWARDED) {
  process.on(sig, () => {
    if (!child.killed) child.kill(sig);
  });
}

child.on("error", (err) => {
  const hint =
    result.source === "env"
      ? `HANDOFF_MCP_BINARY_PATH points at "${result.binary}", which could not be executed.`
      : `The prebuilt binary at "${result.binary}" could not be executed.` +
        "\nIf this is a musl-based system (Alpine), the published binaries are" +
        "\nglibc-linked and will not run; build from source with" +
        "\n`cargo install handoff-mcp` and set HANDOFF_MCP_BINARY_PATH.";
  console.error(`handoff-mcp: ${err.message}\n\n${hint}`);
  process.exit(1);
});

child.on("close", (code, signal) => {
  // Re-raising the signal is what makes `kill` behave as though the wrapper
  // were not there: the parent shell sees the process die from the signal
  // rather than from a plain exit code. The default handler must be restored
  // first, or the forwarding handler above would swallow it.
  if (signal) {
    process.removeAllListeners(signal);
    process.kill(process.pid, signal);
    return;
  }
  process.exit(code == null ? 1 : code);
});
