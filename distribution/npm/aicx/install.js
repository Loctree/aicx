#!/usr/bin/env node

const { existsSync } = require("fs");
const { spawnSync } = require("child_process");
const { getBinaryPath } = require("./index.js");

function validateBinary(binaryName) {
  try {
    const binaryPath = getBinaryPath(binaryName);
    if (!existsSync(binaryPath)) {
      console.warn(`Warning: ${binaryName} binary not found at ${binaryPath}`);
      return;
    }

    const result = spawnSync(binaryPath, ["--version"], { encoding: "utf8" });
    if (result.status === 0) {
      console.log(`${binaryName} binary installed successfully: ${result.stdout.trim()}`);
    } else {
      console.warn(`Warning: ${binaryName} binary may not be working correctly`);
    }
  } catch (error) {
    console.warn(`Warning: Could not verify ${binaryName}: ${error.message}`);
  }
}

validateBinary("aicx");
validateBinary("aicx-mcp");
