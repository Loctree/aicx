#!/usr/bin/env node

const { accessSync, constants, existsSync } = require("fs");
const { getBinaryPath } = require("./index.js");

function validateBinary(binaryName) {
  try {
    const binaryPath = getBinaryPath(binaryName);
    if (!existsSync(binaryPath)) {
      console.warn(`Warning: ${binaryName} binary not found at ${binaryPath}`);
      return;
    }

    accessSync(binaryPath, constants.X_OK);
    console.log(`${binaryName} binary installed successfully at ${binaryPath}`);
  } catch (error) {
    console.warn(`Warning: Could not verify ${binaryName}: ${error.message}`);
  }
}

validateBinary("aicx");
validateBinary("aicx-mcp");
