#!/usr/bin/env node
/**
 * Verifies that the aicx npm wrapper and platform packages match the current
 * GitHub Release asset contract.
 */

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const ROOT = path.dirname(__filename);

const VERSION_ARG = process.argv[2] || null;
const WRAPPER = {
  path: path.join(ROOT, "aicx", "package.json"),
  name: "@loctree/aicx",
};
// assetTriple reflects the cleaned name produced by release_bundle.sh
// (`-unknown-` stripped from linux). archiveExt: macOS+Windows ship .zip
// (Apple notarized .zip for darwin, plain .zip for windows); Linux .tar.gz.
const PLATFORMS = [
  {
    key: "darwin-arm64",
    packageName: "@loctree/aicx-darwin-arm64",
    assetTriple: "aarch64-apple-darwin",
    archiveExt: "zip",
    os: "darwin",
    cpu: "arm64",
  },
  {
    key: "linux-x64-gnu",
    packageName: "@loctree/aicx-linux-x64-gnu",
    assetTriple: "x86_64-linux-gnu",
    archiveExt: "tar.gz",
    os: "linux",
    cpu: "x64",
    libc: "glibc",
  },
  {
    key: "win32-x64-gnu",
    packageName: "@loctree/aicx-win32-x64-gnu",
    assetTriple: "x86_64-pc-windows-msvc",
    archiveExt: "zip",
    os: "win32",
    cpu: "x64",
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
  if (actual !== expected) {
    fail(`${label}: expected ${expected}, found ${actual}`);
  }
}

function assertIncludes(label, text, needle) {
  if (!text.includes(needle)) {
    fail(`${label}: missing ${needle}`);
  }
}

function assertNotIncludes(label, text, needle) {
  if (text.includes(needle)) {
    fail(`${label}: unexpected ${needle}`);
  }
}

const wrapper = readJson(WRAPPER.path);
assertEqual("wrapper name", wrapper.name, WRAPPER.name);

const version = VERSION_ARG || wrapper.version;
assertEqual("wrapper version", wrapper.version, version);

const optionalDependencies = wrapper.optionalDependencies || {};
const expectedOptionalDependencyNames = PLATFORMS.map((platform) => platform.packageName).sort();
const actualOptionalDependencyNames = Object.keys(optionalDependencies).sort();
assertEqual(
  "optional dependency set",
  actualOptionalDependencyNames.join(","),
  expectedOptionalDependencyNames.join(",")
);

for (const packageName of expectedOptionalDependencyNames) {
  assertEqual(`optional dependency ${packageName}`, optionalDependencies[packageName], version);
}

for (const platform of PLATFORMS) {
  const packageJsonPath = path.join(ROOT, "aicx", "platform-packages", platform.key, "package.json");
  const postinstallPath = path.join(ROOT, "aicx", "platform-packages", platform.key, "postinstall.js");
  const pkg = readJson(packageJsonPath);
  const postinstall = fs.readFileSync(postinstallPath, "utf8");

  assertEqual(`${platform.key} package name`, pkg.name, platform.packageName);
  assertEqual(`${platform.key} package version`, pkg.version, version);
  assertIncludes(`${platform.key} package os`, JSON.stringify(pkg.os || []), platform.os);
  assertIncludes(`${platform.key} package cpu`, JSON.stringify(pkg.cpu || []), platform.cpu);
  if (platform.libc) {
    assertIncludes(`${platform.key} package libc`, JSON.stringify(pkg.libc || []), platform.libc);
    assertNotIncludes(`${platform.key} package libc`, JSON.stringify(pkg.libc || []), "musl");
  }
  assertIncludes(`${platform.key} postinstall`, postinstall, platform.packageName);
  assertIncludes(
    `${platform.key} postinstall`,
    postinstall,
    `aicx-v\${VERSION}-${platform.assetTriple}-slim.${platform.archiveExt}`
  );
  assertIncludes(
    `${platform.key} postinstall`,
    postinstall,
    `aicx-v\${VERSION}-${platform.assetTriple}-slim`
  );
  // Loctree releases never ship `-unsigned` assets — every archive is
  // GPG-detached (and macOS additionally Apple-codesigned + notarized).
  assertNotIncludes(`${platform.key} postinstall`, postinstall, "slim-unsigned");
}

if (process.exitCode) {
  process.exit(process.exitCode);
}

console.log(`aicx npm metadata verified for ${version}`);
