#!/usr/bin/env node

const { execFileSync, spawnSync } = require("child_process");
const { existsSync, readFileSync, realpathSync } = require("fs");
const { homedir } = require("os");
const { dirname, isAbsolute, relative, sep } = require("path");

const ISSUES_URL = "https://github.com/Loctree/aicx/issues";
const RELEASES_URL = "https://github.com/Loctree/aicx/releases";

// One entry per published platform package. Keep in sync with
// distribution/npm/verify-metadata.mjs; tools/npm_publish_contract_selftest.py
// fails when a platform directory and this map drift apart.
const PLATFORM_PACKAGES = Object.freeze({
  "darwin-arm64": Object.freeze({
    name: "@loctree/aicx-darwin-arm64",
    label: "macOS, Apple Silicon",
  }),
  "darwin-x64": Object.freeze({
    name: "@loctree/aicx-darwin-x64",
    label: "macOS, Intel",
  }),
  "linux-arm64-gnu": Object.freeze({
    name: "@loctree/aicx-linux-arm64-gnu",
    label: "Linux arm64, glibc",
  }),
  "linux-x64-gnu": Object.freeze({
    name: "@loctree/aicx-linux-x64-gnu",
    label: "Linux x64, glibc",
  }),
  "win32-x64-gnu": Object.freeze({
    name: "@loctree/aicx-win32-x64-gnu",
    label: "Windows x64",
  }),
});

const BINARY_FILENAMES = Object.freeze({
  aicx: process.platform === "win32" ? "aicx.exe" : "aicx",
  "aicx-mcp": process.platform === "win32" ? "aicx-mcp.exe" : "aicx-mcp",
});

// A failure the wrapper can explain to the person at the terminal: no binary
// exists for this host, or the platform package did not land on disk. runCli
// prints message and exits non-zero instead of dumping a Node stack trace.
class AicxPlatformError extends Error {
  constructor(message, options) {
    super(message, options);
    this.name = "AicxPlatformError";
    this.exitCode = 1;
  }
}

function isMuslLibc() {
  try {
    const lddVersion = spawnSync("ldd", ["--version"], { encoding: "utf8" });
    const output = `${lddVersion.stdout || ""}\n${lddVersion.stderr || ""}`;
    return output.includes("musl");
  } catch (error) {
    return false;
  }
}

function detectHost() {
  const host = {
    platform: process.platform,
    arch: process.arch,
    libc: null,
  };
  if (host.platform === "linux") {
    host.libc = isMuslLibc() ? "musl" : "glibc";
  }
  return host;
}

function platformKeyFor(host) {
  const { platform, arch, libc } = host;
  const knownArch = arch === "x64" || arch === "arm64";

  if (platform === "linux" && knownArch && libc === "glibc") {
    return `linux-${arch}-gnu`;
  }
  if (platform === "darwin" && knownArch) {
    return `darwin-${arch}`;
  }
  if (platform === "win32" && arch === "x64") {
    // The legacy npm package key ends in gnu, but its payload is the MSVC build.
    return "win32-x64-gnu";
  }
  return null;
}

function describeHost(host) {
  const lines = [
    `  process.platform: ${host.platform}`,
    `  process.arch:     ${host.arch}`,
  ];
  if (host.libc) lines.push(`  libc:             ${host.libc}`);
  return lines.join("\n");
}

function supportedPlatformsList() {
  return Object.entries(PLATFORM_PACKAGES)
    .map(([key, { label }]) => `  ${key.padEnd(16)} ${label}`)
    .join("\n");
}

function unsupportedPlatformError(host) {
  return new AicxPlatformError(
    `[aicx] No prebuilt aicx binary for this platform.\n` +
    `${describeHost(host)}\n` +
    `Supported platforms:\n${supportedPlatformsList()}\n` +
    `Prebuilt downloads: ${RELEASES_URL}\n` +
    `Missing your platform? Open an issue: ${ISSUES_URL}`
  );
}

function missingPlatformPackageError(host, packageName, detail, options) {
  return new AicxPlatformError(
    `[aicx] ${detail}\n` +
    `${describeHost(host)}\n` +
    `Expected platform package: ${packageName}\n` +
    `Usual causes: npm skipped optionalDependencies (--omit=optional, --no-optional),\n` +
    `or node_modules was installed on a different OS/CPU than the one running it\n` +
    `(for example a macOS node_modules copied into a Linux container).\n` +
    `Reinstall on this machine: npm install @loctree/aicx\n` +
    `Prebuilt downloads: ${RELEASES_URL}\n` +
    `Still failing? Open an issue: ${ISSUES_URL}`,
    options
  );
}

function getPlatformKey() {
  return platformKeyFor(detectHost());
}

function getPlatformPackageName() {
  return getPlatformPackage().name;
}

function getPlatformPackage(host = detectHost()) {
  const platformKey = platformKeyFor(host);
  const platformPackage = platformKey && PLATFORM_PACKAGES[platformKey];
  if (!platformPackage) {
    throw unsupportedPlatformError(host);
  }
  return platformPackage;
}

function getBinaryFileName(binaryName) {
  const binaryFileName = BINARY_FILENAMES[binaryName];
  if (!binaryFileName) {
    throw new Error(`Unsupported binary: ${binaryName}. Expected "aicx" or "aicx-mcp".`);
  }
  return binaryFileName;
}

function resolvePlatformPackageRoot(packageName, searchRoot = __dirname) {
  const manifestPath = require.resolve(`${packageName}/package.json`, {
    paths: [searchRoot],
  });
  return realpathSync(dirname(manifestPath));
}

function assertContainedPath(rootPath, candidatePath) {
  const rel = relative(rootPath, candidatePath);
  if (rel === "" || rel === ".." || rel.startsWith(`..${sep}`) || isAbsolute(rel)) {
    throw new Error(`Resolved binary path escapes package root: ${candidatePath}`);
  }
}

function resolvePlatformBinaryPath(packageRoot, binaryFileName) {
  const allowedBinaryNames = new Set(Object.values(BINARY_FILENAMES));
  if (!allowedBinaryNames.has(binaryFileName)) {
    throw new Error(`Refusing unexpected platform binary name: ${binaryFileName}`);
  }

  // binaryFileName is a member of the closed allowlist above. Keep the final
  // path construction literal so static security analyzers can prove that no
  // untrusted path segment reaches the filesystem.
  let binaryPath;
  if (binaryFileName === "aicx") {
    binaryPath = `${packageRoot}${sep}bin${sep}aicx`;
  } else if (binaryFileName === "aicx-mcp") {
    binaryPath = `${packageRoot}${sep}bin${sep}aicx-mcp`;
  } else if (binaryFileName === "aicx.exe") {
    binaryPath = `${packageRoot}${sep}bin${sep}aicx.exe`;
  } else {
    binaryPath = `${packageRoot}${sep}bin${sep}aicx-mcp.exe`;
  }
  assertContainedPath(packageRoot, binaryPath);
  return binaryPath;
}

function getBinaryPath(binaryName) {
  const host = detectHost();
  const platformPackage = getPlatformPackage(host);
  const packageName = platformPackage.name;
  const resolvedBinaryName = getBinaryFileName(binaryName);
  let packageRoot;

  try {
    packageRoot = resolvePlatformPackageRoot(packageName);
  } catch (error) {
    throw missingPlatformPackageError(
      host,
      packageName,
      `${packageName} is not installed or cannot be resolved.`,
      { cause: error }
    );
  }

  const binaryPath = resolvePlatformBinaryPath(packageRoot, resolvedBinaryName);

  if (!existsSync(binaryPath)) {
    throw missingPlatformPackageError(
      host,
      packageName,
      `${binaryName} binary not found at ${binaryPath}.`
    );
  }

  const realPackageRoot = realpathSync(packageRoot);
  const realBinaryPath = realpathSync(binaryPath);
  assertContainedPath(realPackageRoot, realBinaryPath);

  return realBinaryPath;
}

function execBinary(binaryName, args = [], options = {}) {
  const binaryPath = getBinaryPath(binaryName);
  const execOptions = {
    stdio: "inherit",
    ...options,
  };

  try {
    return execFileSync(binaryPath, args, execOptions);
  } catch (error) {
    const failure = childFailureExit(error);
    if (failure) {
      if (failure.line) process.stderr.write(failure.line);
      process.exit(failure.code);
    }
    throw error;
  }
}

// Node sets status to null when the child dies by signal, and process.exit(null)
// is exit 0. macOS 27 reports a rejected Mach-O as SIGKILL with no stdio.
function childFailureExit(error) {
  if (!error || typeof error !== "object") return null;
  if (error.signal) {
    const signalNumber = require("os").constants.signals[error.signal];
    const code = Number.isInteger(signalNumber) ? 128 + signalNumber : 1;
    const signatureNote = error.signal === "SIGKILL" ? " (code signature invalid)" : "";
    return {
      code,
      line: `[aicx] native binary died from signal ${error.signal}${signatureNote}\n`,
    };
  }
  if (typeof error.status === "number") {
    return { code: error.status, line: "" };
  }
  return null;
}

function plistHost(plist) {
  const match = String(plist).match(/<string>--host<\/string>\s*<string>([^<]*)<\/string>/);
  return match ? match[1] : "";
}

// A migrated LaunchAgent is the loopback reader: host is loopback, auth is
// not required, and the embedded writer is not enabled. The deprecated reader
// flag is optional because current binaries are reader-only by default.
function plistMatchesLoopbackContract(plist) {
  const host = plistHost(plist);
  const loopback = host === "127.0.0.1" || host === "localhost" || host === "::1";
  return (
    loopback &&
    plist.includes("--no-require-auth") &&
    !plist.includes("--experimental-auto-refresh")
  );
}

function execBinarySync(binaryName, args = []) {
  const binaryPath = getBinaryPath(binaryName);

  try {
    return execFileSync(binaryPath, args, { encoding: "utf8" });
  } catch (error) {
    if (error.stdout) {
      return error.stdout;
    }
    throw error;
  }
}

function execAicx(args = [], options = {}) {
  maybePrintRuntimeMigrationHint(args);
  return execBinary("aicx", args, options);
}

function maybePrintRuntimeMigrationHint(args = []) {
  if (process.platform !== "darwin" || args.includes("--repair-runtime")) {
    return;
  }
  const launchAgents = `${homedir()}${sep}Library${sep}LaunchAgents`;
  const plistPath = [
    `${launchAgents}${sep}com.loctree.aicx.mcp.plist`,
    `${launchAgents}${sep}io.vetcoders.aicx.mcp.plist`,
  ].find(existsSync);
  if (!plistPath) {
    return;
  }
  try {
    if (!plistMatchesLoopbackContract(readFileSync(plistPath, "utf8"))) {
      process.stderr.write(
        "[aicx] Background runtime needs a one-time migration: aicx doctor --repair-runtime\n"
      );
    }
  } catch (error) {
    // The native CLI remains authoritative. A best-effort hint must never
    // prevent a command from running because a plist became unreadable.
  }
}

function execAicxSync(args = []) {
  return execBinarySync("aicx", args);
}

function execAicxMcp(args = [], options = {}) {
  return execBinary("aicx-mcp", args, options);
}

function execAicxMcpSync(args = []) {
  return execBinarySync("aicx-mcp", args);
}

// Entry point for the bin/ shims. Platform problems end as a plain message on
// stderr and a non-zero exit; anything else keeps its original stack trace.
function runCli(binaryName, args = []) {
  const run = binaryName === "aicx-mcp" ? execAicxMcp : execAicx;
  try {
    run(args);
  } catch (error) {
    if (error instanceof AicxPlatformError) {
      process.stderr.write(`${error.message}\n`);
      process.exit(error.exitCode);
    }
    throw error;
  }
}

module.exports = {
  execAicx,
  execAicxSync,
  execAicxMcp,
  execAicxMcpSync,
  AicxPlatformError,
  PLATFORM_PACKAGES,
  detectHost,
  getBinaryPath,
  getPlatformPackageName,
  platformKeyFor,
  runCli,
  maybePrintRuntimeMigrationHint,
  plistMatchesLoopbackContract,
  childFailureExit,
  resolvePlatformBinaryPath,
  resolvePlatformPackageRoot,
};

if (require.main === module) {
  process.env.AICX_RUNTIME_BIN ||= process.argv[1];
  runCli("aicx", process.argv.slice(2));
}
