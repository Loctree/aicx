#!/usr/bin/env node
/**
 * sync-version.mjs
 *
 * Bump the shared version across the wrapper and platform package manifests in
 * distribution/npm:
 *
 *   distribution/npm/aicx/package.json
 *   distribution/npm/aicx/platform-packages/{platform}/package.json
 *
 * Usage:
 *   node distribution/npm/sync-version.mjs <version>
 *   node distribution/npm/sync-version.mjs --check
 *   node distribution/npm/sync-version.mjs --check <version>
 */

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const ROOT = __dirname;

const WRAPPERS = ["aicx"];
const PLATFORMS = ["darwin-arm64", "darwin-x64", "linux-x64-gnu", "linux-x64-musl"];

function allPackageJsonPaths() {
  const paths = [];
  for (const wrapper of WRAPPERS) {
    paths.push(path.join(ROOT, wrapper, "package.json"));
    for (const plat of PLATFORMS) {
      paths.push(path.join(ROOT, wrapper, "platform-packages", plat, "package.json"));
    }
  }
  return paths;
}

function readJson(filePath) {
  return JSON.parse(fs.readFileSync(filePath, "utf8"));
}

function writeJson(filePath, value) {
  fs.writeFileSync(filePath, `${JSON.stringify(value, null, 2)}\n`);
}

function bump(version) {
  const paths = allPackageJsonPaths();
  const missing = paths.filter((filePath) => !fs.existsSync(filePath));
  if (missing.length) {
    console.error("Missing package.json files:");
    for (const missingPath of missing) {
      console.error(`  ${missingPath}`);
    }
    process.exit(2);
  }

  for (const filePath of paths) {
    const pkg = readJson(filePath);
    pkg.version = version;
    if (pkg.optionalDependencies) {
      for (const dep of Object.keys(pkg.optionalDependencies)) {
        if (dep.startsWith("@loctree/")) {
          pkg.optionalDependencies[dep] = version;
        }
      }
    }
    writeJson(filePath, pkg);
  }

  console.log(`distribution/npm synced to ${version} across ${paths.length} package.json files`);
}

function check(expectedVersion) {
  const paths = allPackageJsonPaths();
  const missing = paths.filter((filePath) => !fs.existsSync(filePath));
  if (missing.length) {
    console.error("Missing package.json files:");
    for (const missingPath of missing) {
      console.error(`  ${missingPath}`);
    }
    process.exit(2);
  }

  let ok = true;
  let firstVersion = null;
  for (const filePath of paths) {
    let pkg;
    try {
      pkg = readJson(filePath);
    } catch (error) {
      console.error(`Invalid JSON in ${filePath}: ${error.message}`);
      ok = false;
      continue;
    }

    if (typeof pkg.version !== "string" || pkg.version.length === 0) {
      console.error(`Missing version in ${filePath}`);
      ok = false;
      continue;
    }

    if (firstVersion === null) {
      firstVersion = pkg.version;
    }

    if (pkg.version !== firstVersion) {
      console.error(`Version mismatch: ${filePath} has ${pkg.version}, expected ${firstVersion}`);
      ok = false;
    }

    if (expectedVersion && pkg.version !== expectedVersion) {
      console.error(`Expected version ${expectedVersion} in ${filePath}, found ${pkg.version}`);
      ok = false;
    }

    if (pkg.optionalDependencies) {
      for (const [dep, depVersion] of Object.entries(pkg.optionalDependencies)) {
        if (dep.startsWith("@loctree/") && depVersion !== pkg.version) {
          console.error(`Dep version mismatch in ${filePath}: ${dep}=${depVersion} but pkg.version=${pkg.version}`);
          ok = false;
        }
      }
    }
  }

  if (!ok) {
    process.exit(1);
  }

  console.log(`distribution/npm: ${paths.length} package.json files all at version ${firstVersion}`);
}

function main() {
  const argv = process.argv.slice(2);
  if (argv.length === 0) {
    console.error("Usage: node distribution/npm/sync-version.mjs <version>");
    console.error("       node distribution/npm/sync-version.mjs --check [version]");
    process.exit(1);
  }

  if (argv[0] === "--check") {
    check(argv[1]);
    return;
  }

  const version = argv[0];
  if (!/^\d+\.\d+\.\d+/.test(version)) {
    console.error(`Not a valid semver-ish version: ${version}`);
    process.exit(1);
  }

  bump(version);
}

main();
