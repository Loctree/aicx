#!/usr/bin/env node

const { accessSync, constants, existsSync } = require("fs");
const { getBinaryPath } = require("./index.js");

function validateBinary(binaryName) {
  try {
    const binaryPath = getBinaryPath(binaryName);
    if (!existsSync(binaryPath)) {
      console.error(`\n[AICX Install Error] ${binaryName} binary not found at ${binaryPath}`);
      return;
    }

    accessSync(binaryPath, constants.X_OK);
    console.log(`${binaryName} binary installed successfully at ${binaryPath}`);
  } catch (error) {
    console.error(`\n[AICX Install Error] Could not verify ${binaryName}:\n${error.message}\n`);
  }
}

validateBinary("aicx");
validateBinary("aicx-mcp");
