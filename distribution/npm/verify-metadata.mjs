#!/usr/bin/env node
/** Verify the script-free npm wrapper and native platform package payloads. */

import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

const ROOT = path.dirname(fileURLToPath(import.meta.url));
const LIFECYCLE_SCRIPTS = ["preinstall", "install", "postinstall", "prepare"];
const VERSION_ARG = process.argv[2] || null;
const platformArg = process.argv.find((arg) => arg.startsWith("--platform="));
const PLATFORM_ARG = platformArg ? platformArg.split("=", 2)[1] : null;
const WRAPPER = {
  path: path.join(ROOT, "aicx", "package.json"),
  name: "@loctree/aicx",
  files: ["README.md", "bin/aicx", "bin/aicx-mcp", "index.d.ts", "index.js"],
};
const PLATFORMS = [
  {
    key: "darwin-arm64",
    packageName: "@loctree/aicx-darwin-arm64",
    os: "darwin",
    cpu: "arm64",
    binaries: ["bin/aicx", "bin/aicx-mcp"],
  },
  {
    key: "linux-x64-gnu",
    packageName: "@loctree/aicx-linux-x64-gnu",
    os: "linux",
    cpu: "x64",
    libc: "glibc",
    binaries: ["bin/aicx", "bin/aicx-mcp"],
  },
  {
    key: "win32-x64-gnu",
    packageName: "@loctree/aicx-win32-x64-gnu",
    os: "win32",
    cpu: "x64",
    binaries: ["bin/aicx.exe", "bin/aicx-mcp.exe"],
  },
];

function fail(message) {
  console.error(message);
  process.exitCode = 1;
}

function readJson(filePath) {
  return JSON.parse(fs.readFileSync(filePath, "utf8"));
}

function assertEqual(label, actual, expected) {
  if (actual !== expected) fail(`${label}: expected ${expected}, found ${actual}`);
}

function assertStringSet(label, actual, expected) {
  assertEqual(label, [...actual].sort().join(","), [...expected].sort().join(","));
}

function assertNoLifecycleScripts(label, pkg) {
  const scripts = pkg.scripts || {};
  for (const name of LIFECYCLE_SCRIPTS) {
    if (Object.hasOwn(scripts, name)) fail(`${label}: lifecycle script ${name} is forbidden`);
  }
}

function verifyPackedFiles(packageRoot, expectedFiles) {
  const npm = process.platform === "win32" ? "npm.cmd" : "npm";
  const result = spawnSync(npm, ["pack", "--dry-run", "--json", "--ignore-scripts"], {
    cwd: packageRoot,
    encoding: "utf8",
  });
  if (result.status !== 0) {
    fail(`npm pack inspection failed in ${packageRoot}: ${result.stderr || result.stdout}`);
    return;
  }
  try {
    const payload = JSON.parse(result.stdout);
    const packed = (payload[0]?.files || []).map((entry) => entry.path);
    assertStringSet(`${path.basename(packageRoot)} packed files`, packed, ["package.json", ...expectedFiles]);
  } catch (error) {
    fail(`npm pack inspection returned invalid JSON in ${packageRoot}: ${error.message}`);
  }
}

function verifyHoistedPlatformResolution() {
  const fixtureRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aicx-npm-resolution-"));
  const wrapperRoot = path.join(fixtureRoot, "node_modules", "@loctree", "aicx");
  const platformRoot = path.join(fixtureRoot, "node_modules", "@loctree", "aicx-darwin-arm64");
  try {
    fs.mkdirSync(wrapperRoot, { recursive: true });
    fs.mkdirSync(platformRoot, { recursive: true });
    fs.writeFileSync(
      path.join(platformRoot, "package.json"),
      `${JSON.stringify({ name: "@loctree/aicx-darwin-arm64", version: "0.0.0" })}\n`,
    );
    const require = createRequire(import.meta.url);
    const { resolvePlatformBinaryPath, resolvePlatformPackageRoot } = require(
      path.join(ROOT, "aicx", "index.js"),
    );
    const resolvedRoot = resolvePlatformPackageRoot("@loctree/aicx-darwin-arm64", wrapperRoot);
    assertEqual("hoisted platform package resolution", resolvedRoot, fs.realpathSync(platformRoot));
    assertEqual(
      "platform binary bin-directory resolution",
      resolvePlatformBinaryPath(resolvedRoot, "aicx"),
      path.join(resolvedRoot, "bin", "aicx"),
    );
    let traversalRejected = false;
    try {
      resolvePlatformBinaryPath(resolvedRoot, "../../escape");
    } catch {
      traversalRejected = true;
    }
    assertEqual("platform binary traversal rejection", traversalRejected, true);
  } catch (error) {
    fail(`hoisted platform package resolution: ${error.message}`);
  } finally {
    fs.rmSync(fixtureRoot, { recursive: true, force: true });
  }
}

const wrapper = readJson(WRAPPER.path);
assertEqual("wrapper name", wrapper.name, WRAPPER.name);
const version = VERSION_ARG || wrapper.version;
assertEqual("wrapper version", wrapper.version, version);
assertNoLifecycleScripts("wrapper", wrapper);
assertStringSet("wrapper files", wrapper.files || [], WRAPPER.files);

const expectedDependencyNames = PLATFORMS.map((platform) => platform.packageName);
const optionalDependencies = wrapper.optionalDependencies || {};
assertStringSet("optional dependency set", Object.keys(optionalDependencies), expectedDependencyNames);
for (const packageName of expectedDependencyNames) {
  assertEqual(`optional dependency ${packageName}`, optionalDependencies[packageName], version);
}
verifyPackedFiles(path.join(ROOT, "aicx"), WRAPPER.files);

const selectedPlatforms = PLATFORM_ARG
  ? PLATFORMS.filter((platform) => platform.key === PLATFORM_ARG)
  : PLATFORMS;
if (selectedPlatforms.length === 0) {
  fail(`unknown platform ${PLATFORM_ARG}; expected ${PLATFORMS.map((platform) => platform.key).join(", ")}`);
}

for (const platform of selectedPlatforms) {
  const packageRoot = path.join(ROOT, "aicx", "platform-packages", platform.key);
  const pkg = readJson(path.join(packageRoot, "package.json"));
  assertEqual(`${platform.key} package name`, pkg.name, platform.packageName);
  assertEqual(`${platform.key} package version`, pkg.version, version);
  assertStringSet(`${platform.key} package os`, pkg.os || [], [platform.os]);
  assertStringSet(`${platform.key} package cpu`, pkg.cpu || [], [platform.cpu]);
  assertNoLifecycleScripts(platform.key, pkg);
  assertStringSet(`${platform.key} files`, pkg.files || [], platform.binaries);
  if (platform.libc) assertStringSet(`${platform.key} package libc`, pkg.libc || [], [platform.libc]);
  if (!platform.libc && Object.hasOwn(pkg, "libc")) fail(`${platform.key}: unexpected libc fence`);

  for (const binary of platform.binaries) {
    const binaryPath = path.join(packageRoot, binary);
    if (!fs.existsSync(binaryPath)) {
      fail(`${platform.key}: missing packaged binary ${binary}`);
      continue;
    }
    const stat = fs.lstatSync(binaryPath);
    if (!stat.isFile() || stat.isSymbolicLink()) fail(`${platform.key}: ${binary} must be a regular file`);
    if (platform.os !== "win32" && (stat.mode & 0o111) === 0) {
      fail(`${platform.key}: ${binary} is not executable (mode ${(stat.mode & 0o777).toString(8)})`);
    }
  }

  if (platform.os !== process.platform || platform.cpu !== process.arch) {
    fail(
      `${platform.key}: version execution requires its native ${platform.os}/${platform.cpu} runner; ` +
      `current host is ${process.platform}/${process.arch}`,
    );
  } else {
    for (const binary of platform.binaries) {
      const binaryPath = path.join(packageRoot, binary);
      if (!fs.existsSync(binaryPath)) continue;
      const result = spawnSync(binaryPath, ["--version"], { encoding: "utf8" });
      const output = `${result.stdout || ""}\n${result.stderr || ""}`;
      const match = output.match(/\b(\d+\.\d+\.\d+)\b/);
      if (result.status !== 0 || match?.[1] !== version) {
        fail(`${platform.key}: ${binary} --version expected ${version}, status=${result.status}, output=${output.trim()}`);
      }
    }
  }
  verifyPackedFiles(packageRoot, platform.binaries);
}

verifyHoistedPlatformResolution();
if (process.exitCode) process.exit(process.exitCode);
console.log(`aicx npm metadata and packed payload verified for ${version}`);
