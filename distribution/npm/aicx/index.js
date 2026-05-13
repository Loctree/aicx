#!/usr/bin/env node

const { execFileSync, spawnSync } = require("child_process");
const { existsSync, realpathSync } = require("fs");
const { isAbsolute, relative, sep } = require("path");

const PLATFORM_PACKAGES = Object.freeze({
  "darwin-arm64": Object.freeze({
    name: "@loctree/aicx-darwin-arm64",
    root: `${__dirname}/node_modules/@loctree/aicx-darwin-arm64`,
  }),
  "linux-x64-gnu": Object.freeze({
    name: "@loctree/aicx-linux-x64-gnu",
    root: `${__dirname}/node_modules/@loctree/aicx-linux-x64-gnu`,
  }),
});

const BINARY_FILENAMES = Object.freeze({
  aicx: process.platform === "win32" ? "aicx.exe" : "aicx",
  "aicx-mcp": process.platform === "win32" ? "aicx-mcp.exe" : "aicx-mcp",
});

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
    if (isMuslLibc()) {
      throw new Error("AICX npm binaries are not published for Linux musl yet.");
    }
    const libc = "gnu";
    return `${platform}-${normalizedArch}-${libc}`;
  }

  if (platform === "darwin") {
    return `${platform}-${normalizedArch}`;
  }

  return null;
}

function getPlatformPackageName() {
  return getPlatformPackage().name;
}

function getPlatformPackage() {
  const platformKey = getPlatformKey();
  if (!platformKey) {
    throw new Error(
      `Unsupported platform: ${process.platform}-${process.arch}.\n` +
      `AICX currently supports macOS (arm64) and Linux (x64 gnu).\n` +
      `Please build from source or download manually from: https://github.com/Loctree/aicx/releases`
    );
  }

  const platformPackage = PLATFORM_PACKAGES[platformKey];
  if (!platformPackage) {
    throw new Error(
      `No package available for platform: ${platformKey}.\n` +
      `Please build from source or download manually from: https://github.com/Loctree/aicx/releases`
    );
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

function assertContainedPath(rootPath, candidatePath) {
  const rel = relative(rootPath, candidatePath);
  if (rel === "" || rel === ".." || rel.startsWith(`..${sep}`) || isAbsolute(rel)) {
    throw new Error(`Resolved binary path escapes package root: ${candidatePath}`);
  }
}

function getBinaryPath(binaryName) {
  const platformPackage = getPlatformPackage();
  const packageName = platformPackage.name;
  const resolvedBinaryName = getBinaryFileName(binaryName);
  const binaryPath = `${platformPackage.root}/${resolvedBinaryName}`;

  if (!existsSync(binaryPath)) {
    throw new Error(
      `${binaryName} binary not found at ${binaryPath}.\n` +
      `This typically happens if npm optionalDependencies failed to install or were skipped.\n` +
      `Please ensure "${packageName}" is installed, or download the binary manually from:\n` +
      `https://github.com/Loctree/aicx/releases`
    );
  }

  const realPackageRoot = realpathSync(platformPackage.root);
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
