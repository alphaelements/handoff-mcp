import assert from "node:assert/strict";
import { test } from "node:test";

import {
  PACKAGE_NAME,
  SUPPORTED,
  platformPackage,
  binaryEntry,
  resolveBinary,
} from "./resolve-binary.js";

// A resolver stub standing in for require.resolve: it "finds" only the ids
// listed, and throws MODULE_NOT_FOUND for everything else the way Node does.
function resolverFor(map) {
  return (id) => {
    if (Object.prototype.hasOwnProperty.call(map, id)) return map[id];
    const e = new Error(`Cannot find module '${id}'`);
    e.code = "MODULE_NOT_FOUND";
    throw e;
  };
}

const alwaysThrows = resolverFor({});

// ============================================================
// platformPackage — the platform/arch → package name mapping
// ============================================================
test("every published target maps to an unscoped package name", () => {
  assert.equal(platformPackage("linux", "x64"), "handoff-mcp-server-linux-x64");
  assert.equal(platformPackage("linux", "arm64"), "handoff-mcp-server-linux-arm64");
  assert.equal(platformPackage("darwin", "x64"), "handoff-mcp-server-darwin-x64");
  assert.equal(platformPackage("darwin", "arm64"), "handoff-mcp-server-darwin-arm64");
  assert.equal(platformPackage("win32", "x64"), "handoff-mcp-server-win32-x64");
  assert.equal(platformPackage("win32", "arm64"), "handoff-mcp-server-win32-arm64");
});

test("the mapping covers exactly the six published targets", () => {
  assert.equal(Object.keys(SUPPORTED).length, 6);
});

test("names are derived from the wrapper package name, so they cannot drift apart", () => {
  for (const suffix of Object.values(SUPPORTED)) {
    const [platform, arch] = suffix.split("-");
    assert.equal(platformPackage(platform, arch), `${PACKAGE_NAME}-${suffix}`);
  }
});

test("an unpublished platform/arch maps to null rather than a guessed name", () => {
  assert.equal(platformPackage("freebsd", "x64"), null);
  assert.equal(platformPackage("linux", "riscv64"), null);
  // linux-x64 musl shares the platform/arch pair with glibc; the mapping is
  // deliberately not libc-aware (see the runtime check in handoff-mcp.js).
  assert.equal(platformPackage("android", "arm64"), null);
});

// ============================================================
// binaryEntry — Windows needs the .exe suffix
// ============================================================
test("Windows resolves the .exe entry, other platforms the bare name", () => {
  assert.equal(binaryEntry("win32"), "handoff-mcp.exe");
  assert.equal(binaryEntry("linux"), "handoff-mcp");
  assert.equal(binaryEntry("darwin"), "handoff-mcp");
});

// ============================================================
// resolveBinary — the env override wins
// ============================================================
test("HANDOFF_MCP_BINARY_PATH overrides package resolution entirely", () => {
  const r = resolveBinary({
    platform: "linux",
    arch: "x64",
    env: { HANDOFF_MCP_BINARY_PATH: "/opt/custom/handoff-mcp" },
    resolve: alwaysThrows, // must not be consulted
  });
  assert.deepEqual(r, {
    ok: true,
    binary: "/opt/custom/handoff-mcp",
    source: "env",
  });
});

test("the override also rescues platforms we publish no binary for", () => {
  const r = resolveBinary({
    platform: "freebsd",
    arch: "x64",
    env: { HANDOFF_MCP_BINARY_PATH: "/usr/local/bin/handoff-mcp" },
    resolve: alwaysThrows,
  });
  assert.equal(r.ok, true);
  assert.equal(r.binary, "/usr/local/bin/handoff-mcp");
});

test("an empty override is ignored rather than resolving to an empty path", () => {
  const r = resolveBinary({
    platform: "linux",
    arch: "x64",
    env: { HANDOFF_MCP_BINARY_PATH: "" },
    resolve: resolverFor({
      "handoff-mcp-server-linux-x64/bin/handoff-mcp": "/nm/pkg/bin/handoff-mcp",
    }),
  });
  assert.equal(r.ok, true);
  assert.equal(r.source, "package");
});

// ============================================================
// resolveBinary — package resolution
// ============================================================
test("resolution goes through require.resolve, never path arithmetic", () => {
  // The returned path is whatever the resolver reports — a pnpm-style
  // symlinked store location that no __dirname join would ever produce.
  const r = resolveBinary({
    platform: "linux",
    arch: "x64",
    env: {},
    resolve: resolverFor({
      "handoff-mcp-server-linux-x64/bin/handoff-mcp":
        "/proj/node_modules/.pnpm/handoff-mcp-server-linux-x64@0.27.0/node_modules/handoff-mcp-server-linux-x64/bin/handoff-mcp",
    }),
  });
  assert.equal(r.ok, true);
  assert.equal(r.source, "package");
  assert.match(r.binary, /\.pnpm/);
});

test("Windows resolves the .exe inside the platform package", () => {
  const r = resolveBinary({
    platform: "win32",
    arch: "arm64",
    env: {},
    resolve: resolverFor({
      "handoff-mcp-server-win32-arm64/bin/handoff-mcp.exe": "C:\\nm\\bin\\handoff-mcp.exe",
    }),
  });
  assert.equal(r.ok, true);
  assert.equal(r.binary, "C:\\nm\\bin\\handoff-mcp.exe");
});

// ============================================================
// resolveBinary — failure messages must be actionable
// ============================================================
test("a missing platform package fails with the omit=optional and lockfile causes", () => {
  const r = resolveBinary({
    platform: "linux",
    arch: "x64",
    env: {},
    resolve: alwaysThrows,
  });
  assert.equal(r.ok, false);
  assert.match(r.message, /handoff-mcp-server-linux-x64/);
  assert.match(r.message, /--omit=optional/);
  assert.match(r.message, /npm\/cli\/issues\/8320/);
  assert.match(r.message, /HANDOFF_MCP_BINARY_PATH/);
});

test("an unsupported platform is reported as unsupported, not as a broken install", () => {
  const r = resolveBinary({
    platform: "freebsd",
    arch: "x64",
    env: {},
    resolve: alwaysThrows,
  });
  assert.equal(r.ok, false);
  assert.match(r.message, /no prebuilt binary for freebsd-x64/);
  // Telling a FreeBSD user to "reinstall" would send them in a loop; the fix
  // is to build from source.
  assert.doesNotMatch(r.message, /--omit=optional/);
  assert.match(r.message, /cargo install/);
  assert.match(r.message, /HANDOFF_MCP_BINARY_PATH/);
});

test("failure messages never claim success by returning a path", () => {
  for (const [platform, arch] of [
    ["freebsd", "x64"],
    ["linux", "x64"],
  ]) {
    const r = resolveBinary({ platform, arch, env: {}, resolve: alwaysThrows });
    assert.equal(r.ok, false);
    assert.equal(r.binary, undefined);
  }
});
