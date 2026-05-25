#!/usr/bin/env node

const { accessSync, constants, existsSync, unlinkSync } = require("fs");
const { execFileSync } = require("child_process");
const { homedir } = require("os");
const { join } = require("path");
const { getBinaryPath } = require("./index.js");

let hasError = false;
const VERSION = require("./package.json").version;

function envFlag(name) {
  return /^(1|true|yes|on)$/i.test(process.env[name] || "");
}

function commandOutput(command, args) {
  try {
    return execFileSync(command, args, { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] }).trim();
  } catch (error) {
    return "";
  }
}

function parseSemver(text) {
  const match = String(text || "").match(/(\d+)\.(\d+)\.(\d+)/);
  return match ? match[0] : "";
}

function compareSemver(left, right) {
  const a = left.split(".").map(Number);
  const b = right.split(".").map(Number);
  if (a.length !== 3 || b.length !== 3 || a.some(Number.isNaN) || b.some(Number.isNaN)) {
    return null;
  }
  for (let i = 0; i < 3; i += 1) {
    if (a[i] < b[i]) return -1;
    if (a[i] > b[i]) return 1;
  }
  return 0;
}

function binaryVersion(binaryPath) {
  if (!existsSync(binaryPath)) return "";
  return parseSemver(commandOutput(binaryPath, ["--version"]));
}

function whichAll(binaryName) {
  const command = process.platform === "win32" ? "where" : "which";
  const args = process.platform === "win32" ? [binaryName] : ["-a", binaryName];
  return commandOutput(command, args).split(/\r?\n/).filter(Boolean);
}

function cleanupShadowDir(dir, targetVersion) {
  const suffix = process.platform === "win32" ? ".exe" : "";
  const candidateAicx = join(dir, `aicx${suffix}`);
  const candidateMcp = join(dir, `aicx-mcp${suffix}`);
  if (!existsSync(candidateAicx)) return;

  const candidateVersion = binaryVersion(candidateAicx);
  const comparison = compareSemver(candidateVersion, targetVersion);
  if (comparison === null || comparison > 0) {
    console.warn(`[AICX npm] Shadow retained at ${candidateAicx} (version: ${candidateVersion || "unknown"})`);
    return;
  }

  for (const path of [candidateAicx, candidateMcp]) {
    if (!existsSync(path)) continue;
    unlinkSync(path);
    console.warn(`[AICX npm] Removed older/equal shadow binary: ${path}`);
  }
}

function scanAicxShadows(installedPath, targetVersion) {
  const pathBinaries = Array.from(new Set(whichAll("aicx")));
  if (pathBinaries.length === 0) return;

  console.warn("[AICX npm] Existing aicx binaries on PATH:");
  for (const path of pathBinaries) {
    const version = commandOutput(path, ["--version"]) || "unknown";
    console.warn(`  ${path} -> ${version}`);
  }

  const resolved = pathBinaries[0];
  if (resolved && resolved !== installedPath) {
    console.warn("[AICX npm] WARNING: PATH may resolve to a different aicx than this npm package.");
    console.warn(`  npm package binary: ${installedPath} -> ${targetVersion}`);
    console.warn(`  PATH resolves to:   ${resolved}`);
    console.warn("  Set AICX_NPM_REPLACE_LOCAL=1 to remove older/equal ~/.local/bin or cargo-bin shadows during npm install.");
  }

  if (envFlag("AICX_NPM_REPLACE_LOCAL")) {
    cleanupShadowDir(join(homedir(), ".local", "bin"), targetVersion);
    cleanupShadowDir(join(homedir(), ".cargo", "bin"), targetVersion);
  }
}

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

scanAicxShadows(getBinaryPath("aicx"), VERSION);
