#!/usr/bin/env node
/** Verify a signed release asset and stage its native binaries for npm pack. */

import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const ROOT = path.dirname(fileURLToPath(import.meta.url));
const [version, platformKey, releaseDirArg, verificationMode = "signed-release"] = process.argv.slice(2);
const PLATFORMS = {
  "darwin-arm64": {
    asset: `aicx-v${version}-aarch64-apple-darwin-slim.zip`,
    binaries: ["aicx", "aicx-mcp"],
  },
  "linux-x64-gnu": {
    asset: `aicx-v${version}-x86_64-linux-gnu-slim.tar.gz`,
    binaries: ["aicx", "aicx-mcp"],
  },
  "win32-x64-gnu": {
    asset: `aicx-v${version}-x86_64-pc-windows-msvc-slim.zip`,
    binaries: ["aicx.exe", "aicx-mcp.exe"],
  },
};

if (!/^\d+\.\d+\.\d+$/.test(version || "")) throw new Error("version must look like x.y.z");
const platform = PLATFORMS[platformKey];
if (!platform) throw new Error(`unsupported platform key: ${platformKey}`);
if (!releaseDirArg) throw new Error("usage: stage-platform-package.mjs <version> <platform> <release-dir>");

const releaseDir = path.resolve(releaseDirArg);
let assetName = platform.asset;
if (
  verificationMode === "local-unsigned-ci" &&
  platformKey === "darwin-arm64" &&
  !fs.existsSync(path.join(releaseDir, assetName))
) {
  assetName = `aicx-v${version}-aarch64-apple-darwin-slim.tar.gz`;
}
const archivePath = path.join(releaseDir, assetName);
const checksumPath = `${archivePath}.sha256`;
const signaturePath = `${archivePath}.asc`;
const publicKeyPath = path.join(releaseDir, "loctree-release-pubkey.asc");
const requiredInputs = [archivePath, checksumPath];
if (verificationMode === "signed-release") requiredInputs.push(signaturePath, publicKeyPath);
if (!new Set(["signed-release", "local-unsigned-ci"]).has(verificationMode)) {
  throw new Error(`unsupported verification mode: ${verificationMode}`);
}
for (const required of requiredInputs) {
  if (!fs.statSync(required).isFile()) throw new Error(`required release input is not a file: ${required}`);
}

const checksumLine = fs.readFileSync(checksumPath, "utf8").trim().split(/\r?\n/, 1)[0];
const checksumMatch = checksumLine.match(/^([a-fA-F0-9]{64})\s+[*]?(.+)$/);
if (!checksumMatch || path.basename(checksumMatch[2]) !== assetName) {
  throw new Error(`invalid checksum sidecar for ${assetName}`);
}
const actualHash = crypto.createHash("sha256").update(fs.readFileSync(archivePath)).digest("hex");
if (actualHash !== checksumMatch[1].toLowerCase()) {
  throw new Error(`SHA-256 mismatch for ${assetName}`);
}

const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aicx-npm-stage-"));
const gpgHome = path.join(tempRoot, "gnupg");
const extractRoot = path.join(tempRoot, "extract");
fs.mkdirSync(gpgHome, { mode: 0o700 });
fs.mkdirSync(extractRoot);

function listFiles(root, output = []) {
  for (const entry of fs.readdirSync(root, { withFileTypes: true })) {
    const entryPath = path.join(root, entry.name);
    if (entry.isSymbolicLink()) throw new Error(`release archive contains a symlink: ${entryPath}`);
    if (entry.isDirectory()) listFiles(entryPath, output);
    if (entry.isFile()) output.push(entryPath);
  }
  return output;
}

// windows-latest resolves `gpg` to Git for Windows' MSYS build. Spawned from
// node (no MSYS shell in between) it receives Windows paths verbatim and treats
// `C:\Users\...` as a relative name — the 0.13.0 publish run died importing the
// release key into `/d/a/aicx/aicx/C:\Users\RUNNER~1\...`. That gpg understands
// POSIX paths, and ships `cygpath` to produce them; a native gpg (Gpg4win) takes
// Windows paths as-is, so convert only when the resolved gpg is the MSYS one.
let gpgWantsPosixPaths;
function gpgPath(candidate) {
  if (process.platform !== "win32") return candidate;
  if (gpgWantsPosixPaths === undefined) {
    let resolved = "";
    try {
      resolved = execFileSync("where.exe", ["gpg"], { encoding: "utf8" }).split(/\r?\n/, 1)[0].trim();
    } catch {
      resolved = "";
    }
    gpgWantsPosixPaths = /[\\/]usr[\\/]bin[\\/]gpg(\.exe)?$/i.test(resolved);
  }
  if (!gpgWantsPosixPaths) return candidate;
  return execFileSync("cygpath", ["-u", candidate], { encoding: "utf8" }).trim();
}

try {
  if (verificationMode === "signed-release") {
    execFileSync("gpg", ["--batch", "--homedir", gpgPath(gpgHome), "--import", gpgPath(publicKeyPath)], {
      stdio: "inherit",
    });
    execFileSync(
      "gpg",
      ["--batch", "--homedir", gpgPath(gpgHome), "--verify", gpgPath(signaturePath), gpgPath(archivePath)],
      { stdio: "inherit" },
    );
  }

  if (assetName.endsWith(".tar.gz")) {
    execFileSync("tar", ["-xzf", archivePath, "-C", extractRoot], { stdio: "inherit" });
  } else if (process.platform === "win32") {
    execFileSync("tar", ["-xf", archivePath, "-C", extractRoot], { stdio: "inherit" });
  } else {
    execFileSync("unzip", ["-q", archivePath, "-d", extractRoot], { stdio: "inherit" });
  }

  const extractedFiles = listFiles(extractRoot);
  const packageRoot = path.join(ROOT, "aicx", "platform-packages", platformKey);
  const packageBin = path.join(packageRoot, "bin");
  fs.rmSync(packageBin, { recursive: true, force: true });
  fs.mkdirSync(packageBin, { recursive: true });
  for (const binary of platform.binaries) {
    const matches = extractedFiles.filter((candidate) => path.basename(candidate) === binary);
    if (matches.length !== 1) {
      throw new Error(`${assetName}: expected exactly one ${binary}, found ${matches.length}`);
    }
    const destination = path.join(packageBin, binary);
    fs.copyFileSync(matches[0], destination);
    if (process.platform !== "win32") fs.chmodSync(destination, 0o755);
  }
} finally {
  fs.rmSync(tempRoot, { recursive: true, force: true });
}

console.log(
  `verified ${assetName} (${verificationMode}) and staged ${platform.binaries.join(", ")} for ${platformKey}`,
);
