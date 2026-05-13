#!/usr/bin/env node

const { accessSync, constants, existsSync } = require("fs");
const { getBinaryPath } = require("./index.js");

let hasError = false;

function validateBinary(binaryName) {
  try {
    const binaryPath = getBinaryPath(binaryName);
    if (!existsSync(binaryPath)) {
      console.error(`\n[AICX Install Error] ${binaryName} binary not found at ${binaryPath}`);
      hasError = true;
      return;
    }

    accessSync(binaryPath, constants.X_OK);
    console.log(`${binaryName} binary installed successfully at ${binaryPath}`);
  } catch (error) {
    console.error(`\n[AICX Install Error] Could not verify ${binaryName}:\n${error.message}\n`);
    hasError = true;
  }
}

validateBinary("aicx");
validateBinary("aicx-mcp");

if (hasError) {
  console.error("\n======================================================================");
  console.error("AICX npm installation failed.");
  console.error("This usually happens because your platform is not supported by our");
  console.error("prebuilt binaries, or npm failed to download optionalDependencies.\n");
  console.error("Supported pre-built platforms:");
  console.error("  - macOS arm64 (Apple Silicon)");
  console.error("  - Linux x64 (GNU libc)\n");
  console.error("If you are on a supported platform, check your network or npm config.");
  console.error("If you are on an unsupported platform (e.g. Windows, Linux musl, macOS Intel),");
  console.error("use a source build as a contributor fallback.\n");
  console.error("To install from source (requires Rust):");
  console.error("  cargo install --git https://github.com/Loctree/aicx.git\n");
  console.error("Alternatively, download a binary manually from:");
  console.error("  https://github.com/Loctree/aicx/releases");
  console.error("======================================================================\n");
  process.exit(1);
}
