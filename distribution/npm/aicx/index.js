#!/usr/bin/env node

const { execFileSync, spawnSync } = require("child_process");
const { existsSync } = require("fs");
const { join } = require("path");

const PLATFORMS = {
  "darwin-arm64": "@loctree/aicx-darwin-arm64",
  "darwin-x64": "@loctree/aicx-darwin-x64",
  "linux-x64-gnu": "@loctree/aicx-linux-x64-gnu",
  "linux-x64-musl": "@loctree/aicx-linux-x64-musl",
};

function isMuslLibc() {
  try {
    const lddVersion = spawnSync("ldd", ["--version"], { encoding: "utf8" });
    const output = `${lddVersion.stdout || ""}\n${lddVersion.stderr || ""}`;
    return output.includes("musl");
  } catch (error) {
    return false;
  }
}

function getPlatformKey() {
  const platform = process.platform;
  const arch = process.arch;

  const archMap = {
    x64: "x64",
    arm64: "arm64",
    aarch64: "arm64",
  };

  const normalizedArch = archMap[arch] || arch;

  if (platform === "linux") {
    const libc = isMuslLibc() ? "musl" : "gnu";
    return `${platform}-${normalizedArch}-${libc}`;
  }

  if (platform === "darwin") {
    return `${platform}-${normalizedArch}`;
  }

  return null;
}

function getPlatformPackageName() {
  const platformKey = getPlatformKey();
  if (!platformKey) {
    throw new Error(`Unsupported platform: ${process.platform}-${process.arch}`);
  }

  const packageName = PLATFORMS[platformKey];
  if (!packageName) {
    throw new Error(`No package available for platform: ${platformKey}`);
  }

  return packageName;
}

function getBinaryPath(binaryName) {
  const packageName = getPlatformPackageName();
  const resolvedBinaryName = process.platform === "win32" ? `${binaryName}.exe` : binaryName;
  const binaryPath = join(__dirname, "node_modules", packageName, resolvedBinaryName);

  if (!existsSync(binaryPath)) {
    throw new Error(
      `${binaryName} binary not found at ${binaryPath}. ` +
      `This may happen if optionalDependencies are disabled. ` +
      `Please ensure "${packageName}" is installed.`
    );
  }

  return binaryPath;
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
    if (error.status !== undefined) {
      process.exit(error.status);
    }
    throw error;
  }
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
  return execBinary("aicx", args, options);
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

module.exports = {
  execAicx,
  execAicxSync,
  execAicxMcp,
  execAicxMcpSync,
  getBinaryPath,
};

if (require.main === module) {
  execAicx(process.argv.slice(2));
}
