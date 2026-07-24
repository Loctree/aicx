#!/usr/bin/env node

const { accessSync, constants, existsSync, unlinkSync } = require("fs");
const { execFileSync } = require("child_process");
const { homedir } = require("os");
const { join } = require("path");
const {
  getBinaryPath,
  getPlatformPackageName,
  resolvePlatformPackageRoot,
} = require("./index.js");

const VERSION = require("./package.json").version;
const PLATFORM_INSTALL_TIMEOUT_MS = 120_000;
const PLATFORM_INSTALL_POLL_MS = 250;

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

// Each scope branch constructs the candidate path with all-literal join args
// (homedir() is a Node builtin, the subdir + binary names are string literals).
// No variable is forwarded into join, so there is no path that an attacker
// could influence — the only choice points are platform (.exe suffix) and
// the closed `scope` enum, both validated locally.
function cleanupShadowDir(scope, targetVersion) {
  const isWin = process.platform === "win32";
  let candidateAicx;
  let candidateMcp;
  if (scope === "local-bin") {
    candidateAicx = isWin
      ? join(homedir(), ".local", "bin", "aicx.exe")
      : join(homedir(), ".local", "bin", "aicx");
    candidateMcp = isWin
      ? join(homedir(), ".local", "bin", "aicx-mcp.exe")
      : join(homedir(), ".local", "bin", "aicx-mcp");
  } else if (scope === "cargo-bin") {
    candidateAicx = isWin
      ? join(homedir(), ".cargo", "bin", "aicx.exe")
      : join(homedir(), ".cargo", "bin", "aicx");
    candidateMcp = isWin
      ? join(homedir(), ".cargo", "bin", "aicx-mcp.exe")
      : join(homedir(), ".cargo", "bin", "aicx-mcp");
  } else {
    return; // unknown scope — refuse to operate
  }
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

function scanBinaryShadows(binaryName, installedPath, targetVersion) {
  const pathBinaries = Array.from(new Set(whichAll(binaryName)));
  if (pathBinaries.length === 0) return;

  console.warn(`[AICX npm] Existing ${binaryName} binaries on PATH:`);
  for (const path of pathBinaries) {
    const version = commandOutput(path, ["--version"]) || "unknown";
    console.warn(`  ${path} -> ${version}`);
  }

  const resolved = pathBinaries[0];
  if (resolved && resolved !== installedPath) {
    console.warn(`[AICX npm] WARNING: PATH may resolve to a different ${binaryName} than this npm package.`);
    console.warn(`  npm package binary: ${installedPath} -> ${targetVersion}`);
    console.warn(`  PATH resolves to:   ${resolved}`);
    console.warn("  Set AICX_NPM_REPLACE_LOCAL=1 to remove older/equal ~/.local/bin or cargo-bin shadows during npm install.");
  }
}

function scanAicxShadows(installedAicxPath, installedMcpPath, targetVersion) {
  scanBinaryShadows("aicx", installedAicxPath, targetVersion);
  scanBinaryShadows("aicx-mcp", installedMcpPath, targetVersion);
  if (envFlag("AICX_NPM_REPLACE_LOCAL")) {
    cleanupShadowDir("local-bin", targetVersion);
    cleanupShadowDir("cargo-bin", targetVersion);
  }
}

function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function waitForPlatformBinaries() {
  const packageName = getPlatformPackageName();

  // npm may run dependency and wrapper postinstall scripts concurrently. Fail
  // immediately when the optional platform package is absent, but when npm has
  // already installed its manifest, allow its downloader to finish atomically.
  resolvePlatformPackageRoot(packageName);

  const deadline = Date.now() + PLATFORM_INSTALL_TIMEOUT_MS;
  let lastError;
  while (Date.now() < deadline) {
    try {
      return {
        aicx: getBinaryPath("aicx"),
        mcp: getBinaryPath("aicx-mcp"),
      };
    } catch (error) {
      lastError = error;
      await delay(PLATFORM_INSTALL_POLL_MS);
    }
  }

  throw new Error(
    `Timed out after ${PLATFORM_INSTALL_TIMEOUT_MS}ms waiting for ${packageName} binaries.\n` +
    `${lastError ? lastError.message : "No binary status was reported."}`
  );
}

async function main() {
  let binaryPaths;
  try {
    binaryPaths = await waitForPlatformBinaries();
    accessSync(binaryPaths.aicx, constants.X_OK);
    accessSync(binaryPaths.mcp, constants.X_OK);
  } catch (error) {
    console.error(`\n[AICX Install Error] Could not verify platform binaries:\n${error.message}\n`);
    console.error("\n======================================================================");
    console.error("AICX npm installation failed.");
    console.error("This usually happens because your platform is not supported by our");
    console.error("prebuilt binaries, or npm failed to download optionalDependencies.\n");
    console.error("Supported pre-built platforms:");
    console.error("  - macOS arm64 (Apple Silicon)");
    console.error("  - Linux x64 (GNU libc)");
    console.error("  - Windows x64 (MSVC)\n");
    console.error("If you are on a supported platform, check your network or npm config.");
    console.error("If you are on an unsupported platform (e.g. Linux musl or macOS Intel),");
    console.error("use a source build as a contributor fallback.\n");
    console.error("To install from source (requires Rust):");
    console.error("  cargo install --git https://github.com/Loctree/aicx.git\n");
    console.error("Alternatively, download a binary manually from:");
    console.error("  https://github.com/Loctree/aicx/releases");
    console.error("======================================================================\n");
    process.exitCode = 1;
    return;
  }

  console.log(`aicx binary installed successfully at ${binaryPaths.aicx}`);
  console.log(`aicx-mcp binary installed successfully at ${binaryPaths.mcp}`);
  scanAicxShadows(binaryPaths.aicx, binaryPaths.mcp, VERSION);
}

main().catch((error) => {
  console.error(`\n[AICX Install Error] Unexpected failure:\n${error.stack || error.message}\n`);
  process.exitCode = 1;
});
