#!/usr/bin/env node
"use strict";

const { execSync } = require("child_process");
const fs = require("fs");
const path = require("path");

const ROOT = path.resolve(__dirname, "..");
const BIN_DIR = path.join(ROOT, "bin");
// Windows needs the .exe suffix, both for the cargo output and for the copy we
// install. Keep this in sync with bin/handoff-mcp.js.
const EXE = process.platform === "win32" ? ".exe" : "";
const BINARY = path.join(BIN_DIR, `handoff-mcp-bin${EXE}`);

if (fs.existsSync(BINARY)) {
  try {
    execSync(`"${BINARY}" --help`, { stdio: "ignore", timeout: 5000 });
    process.exit(0);
  } catch {
    // prebuilt binary exists but can't run on this platform — rebuild
    fs.unlinkSync(BINARY);
  }
}

try {
  execSync("cargo --version", { stdio: "ignore" });
} catch {
  console.error(
    "Error: Rust toolchain not found.\n" +
      "handoff-mcp-server requires Rust to build from source.\n" +
      "Install Rust: https://rustup.rs/"
  );
  process.exit(1);
}

console.log("Building handoff-mcp from source...");
try {
  execSync("cargo build --release", { cwd: ROOT, stdio: "inherit" });
} catch {
  console.error("Error: cargo build failed.");
  process.exit(1);
}

const built = path.join(ROOT, "target", "release", `handoff-mcp${EXE}`);
if (!fs.existsSync(built)) {
  console.error("Error: binary not found after build.");
  process.exit(1);
}

fs.mkdirSync(BIN_DIR, { recursive: true });
fs.copyFileSync(built, BINARY);
// Windows has no executable bit; chmod there only toggles the read-only flag.
if (process.platform !== "win32") {
  fs.chmodSync(BINARY, 0o755);
}
console.log("handoff-mcp installed successfully.");
