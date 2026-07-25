#!/usr/bin/env node
"use strict";

const { execFileSync } = require("child_process");
const path = require("path");
const fs = require("fs");

// Windows requires the .exe suffix to execute the binary.
const binary = path.join(
  __dirname,
  process.platform === "win32" ? "handoff-mcp-bin.exe" : "handoff-mcp-bin"
);

if (!fs.existsSync(binary)) {
  console.error(
    "handoff-mcp binary not found. Try reinstalling: npm install -g handoff-mcp-server"
  );
  process.exit(1);
}

try {
  execFileSync(binary, process.argv.slice(2), { stdio: "inherit" });
} catch (e) {
  if (e.status != null) process.exit(e.status);
  process.exit(1);
}
