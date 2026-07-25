"use strict";

// Resolution logic for the prebuilt binary shipped by the per-platform
// optionalDependencies (handoff-mcp-server-<platform>-<arch>).
//
// Kept separate from handoff-mcp.js so the mapping and the error messages can
// be unit-tested without spawning a real binary.

const PACKAGE_NAME = "handoff-mcp-server";

// Platform/arch pairs we publish a prebuilt binary for. Keys are
// `${process.platform}-${process.arch}`; see .github/workflows/release.yml for
// the matching build matrix.
const SUPPORTED = {
  "linux-x64": "linux-x64",
  "linux-arm64": "linux-arm64",
  "darwin-x64": "darwin-x64",
  "darwin-arm64": "darwin-arm64",
  "win32-x64": "win32-x64",
  "win32-arm64": "win32-arm64",
};

/** Name of the platform package for a given platform/arch, or null if unsupported. */
function platformPackage(platform, arch) {
  const suffix = SUPPORTED[`${platform}-${arch}`];
  return suffix ? `${PACKAGE_NAME}-${suffix}` : null;
}

/** Path of the binary *inside* a platform package. Windows needs the .exe suffix. */
function binaryEntry(platform) {
  return platform === "win32" ? "handoff-mcp.exe" : "handoff-mcp";
}

/**
 * Locate the handoff-mcp binary.
 *
 * Order:
 *   1. HANDOFF_MCP_BINARY_PATH override (esbuild's ESBUILD_BINARY_PATH
 *      equivalent) — lets users point at a self-built binary on platforms we
 *      do not publish, and lets tests run without installing anything.
 *   2. require.resolve() into the platform package. Never path arithmetic:
 *      npm hoisting and pnpm's symlinked store both move node_modules around,
 *      and require.resolve is the only thing that follows those layouts.
 *
 * @param {object} [opts]
 * @param {string} [opts.platform] defaults to process.platform
 * @param {string} [opts.arch] defaults to process.arch
 * @param {object} [opts.env] defaults to process.env
 * @param {(id: string) => string} [opts.resolve] defaults to require.resolve
 * @returns {{ ok: true, binary: string, source: "env" | "package" }
 *          | { ok: false, message: string }}
 */
function resolveBinary(opts = {}) {
  const platform = opts.platform || process.platform;
  const arch = opts.arch || process.arch;
  const env = opts.env || process.env;
  const resolve = opts.resolve || require.resolve;

  const override = env.HANDOFF_MCP_BINARY_PATH;
  if (override) {
    // Trusted as-is: an explicit override that does not exist should surface
    // as a spawn error naming the path the user asked for, not as a fallback
    // to some other binary they did not ask for.
    return { ok: true, binary: override, source: "env" };
  }

  const pkg = platformPackage(platform, arch);
  if (!pkg) {
    return {
      ok: false,
      message: unsupportedMessage(platform, arch),
    };
  }

  try {
    return {
      ok: true,
      binary: resolve(`${pkg}/bin/${binaryEntry(platform)}`),
      source: "package",
    };
  } catch (e) {
    return { ok: false, message: missingPackageMessage(pkg, e) };
  }
}

function unsupportedMessage(platform, arch) {
  return [
    `handoff-mcp: no prebuilt binary for ${platform}-${arch}.`,
    "",
    `Prebuilt binaries are published for: ${Object.keys(SUPPORTED).join(", ")}.`,
    "",
    "To run on this platform, build from source and point the wrapper at it:",
    "  cargo install handoff-mcp",
    "  export HANDOFF_MCP_BINARY_PATH=$(command -v handoff-mcp)",
  ].join("\n");
}

function missingPackageMessage(pkg, cause) {
  return [
    `handoff-mcp: the platform package "${pkg}" is not installed.`,
    "",
    "This usually means one of:",
    "  - installed with --omit=optional / --no-optional (the prebuilt binary",
    "    ships as an optional dependency; reinstall without that flag)",
    "  - a lockfile generated on a different platform was installed with",
    "    `npm ci` (see https://github.com/npm/cli/issues/8320); delete",
    "    node_modules and package-lock.json, then reinstall",
    "",
    `Reinstall with:  npm install -g ${PACKAGE_NAME}`,
    `Or set HANDOFF_MCP_BINARY_PATH to a binary you built yourself.`,
    "",
    `(resolution error: ${cause && cause.message ? cause.message : cause})`,
  ].join("\n");
}

module.exports = {
  PACKAGE_NAME,
  SUPPORTED,
  platformPackage,
  binaryEntry,
  resolveBinary,
};
