#!/usr/bin/env node
"use strict";

// Generate the six per-platform npm packages that carry the prebuilt binaries.
//
// Usage:
//   node scripts/build-npm-packages.js --target <triple> --binary <path> [--outdir npm]
//   node scripts/build-npm-packages.js --manifest-only [--outdir npm]
//
// release.yml calls the first form once per build-matrix leg (each runner only
// has its own binary), then publishes every directory under --outdir.
//
// The wrapper package is NOT generated here; it is the repo root package.json.

const fs = require("fs");
const path = require("path");

const ROOT = path.resolve(__dirname, "..");

// Rust target triple -> npm platform package identity. Must stay in sync with
// bin/resolve-binary.js SUPPORTED and the release.yml build matrix.
const TARGETS = {
  "x86_64-unknown-linux-gnu": { os: "linux", cpu: "x64", libc: "glibc" },
  "aarch64-unknown-linux-gnu": { os: "linux", cpu: "arm64", libc: "glibc" },
  "x86_64-apple-darwin": { os: "darwin", cpu: "x64" },
  "aarch64-apple-darwin": { os: "darwin", cpu: "arm64" },
  "x86_64-pc-windows-msvc": { os: "win32", cpu: "x64" },
  "aarch64-pc-windows-msvc": { os: "win32", cpu: "arm64" },
};

function parseArgs(argv) {
  const args = { outdir: "npm" };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--target") args.target = argv[++i];
    else if (a === "--binary") args.binary = argv[++i];
    else if (a === "--outdir") args.outdir = argv[++i];
    else if (a === "--manifest-only") args.manifestOnly = true;
    else die(`unknown argument: ${a}`);
  }
  return args;
}

function die(msg) {
  console.error(`build-npm-packages: ${msg}`);
  process.exit(1);
}

function rootPackage() {
  return JSON.parse(fs.readFileSync(path.join(ROOT, "package.json"), "utf8"));
}

/** Build the package.json body for one platform package. */
function manifestFor(target, version, root) {
  const t = TARGETS[target];
  const name = `${root.name}-${t.os}-${t.cpu}`;
  const manifest = {
    name,
    version,
    description: `Prebuilt handoff-mcp binary for ${t.os}-${t.cpu}`,
    license: root.license,
    repository: root.repository,
    homepage: root.homepage,
    os: [t.os],
    cpu: [t.cpu],
    // Yarn PnP keeps dependencies zipped; a binary inside a zip cannot be
    // executed, so this package must always be unpacked to disk.
    preferUnplugged: true,
    files: ["bin/"],
  };
  // npm's `libc` enforcement is not well documented, so it is a hint here, not
  // the guarantee — bin/handoff-mcp.js also reports a musl-specific message if
  // a glibc binary fails to exec.
  if (t.libc) manifest.libc = [t.libc];
  return manifest;
}

function binaryName(target) {
  return TARGETS[target].os === "win32" ? "handoff-mcp.exe" : "handoff-mcp";
}

function writePackage(target, version, root, outdir, binarySrc) {
  const manifest = manifestFor(target, version, root);
  const dir = path.join(outdir, manifest.name);
  fs.mkdirSync(path.join(dir, "bin"), { recursive: true });
  fs.writeFileSync(
    path.join(dir, "package.json"),
    JSON.stringify(manifest, null, 2) + "\n"
  );
  fs.copyFileSync(path.join(ROOT, "LICENSE"), path.join(dir, "LICENSE"));
  fs.writeFileSync(
    path.join(dir, "README.md"),
    `# ${manifest.name}\n\n` +
      `Prebuilt \`handoff-mcp\` binary for ${manifest.os[0]}-${manifest.cpu[0]}.\n\n` +
      `This package is installed automatically as an optional dependency of ` +
      `[\`${root.name}\`](https://www.npmjs.com/package/${root.name}). ` +
      `Install that instead.\n`
  );

  if (binarySrc) {
    const dest = path.join(dir, "bin", binaryName(target));
    fs.copyFileSync(binarySrc, dest);
    // npm preserves the executable bit in the tarball; without it the wrapper
    // resolves the path and then fails with EACCES.
    fs.chmodSync(dest, 0o755);
    const size = fs.statSync(dest).size;
    console.log(`${manifest.name}: ${dest} (${(size / 1e6).toFixed(1)} MB)`);
  } else {
    console.log(`${manifest.name}: manifest only`);
  }
  return dir;
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  const root = rootPackage();
  const version = root.version;

  // The wrapper pins each platform package to an exact version. If that pin
  // and the version we are about to build disagree, every install resolves to
  // a different binary than the one released — fail loudly instead.
  for (const target of Object.keys(TARGETS)) {
    const name = manifestFor(target, version, root).name;
    const pinned = (root.optionalDependencies || {})[name];
    if (pinned !== version) {
      die(
        `package.json optionalDependencies["${name}"] is "${pinned}" but the ` +
          `version being built is "${version}". They must match exactly.`
      );
    }
  }

  const outdir = path.resolve(ROOT, args.outdir);

  if (args.manifestOnly) {
    for (const target of Object.keys(TARGETS)) {
      writePackage(target, version, root, outdir, null);
    }
    return;
  }

  if (!args.target) die("--target is required (or use --manifest-only)");
  if (!TARGETS[args.target]) {
    die(
      `unknown target "${args.target}". Known: ${Object.keys(TARGETS).join(", ")}`
    );
  }
  if (!args.binary) die("--binary is required");
  if (!fs.existsSync(args.binary)) die(`binary not found: ${args.binary}`);

  writePackage(args.target, version, root, outdir, path.resolve(args.binary));
}

if (require.main === module) main();

module.exports = { TARGETS, manifestFor, binaryName };
