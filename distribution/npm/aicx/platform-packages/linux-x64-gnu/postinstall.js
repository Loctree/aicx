#!/usr/bin/env node

const https = require("https");
const crypto = require("crypto");
const {
  chmodSync,
  copyFileSync,
  existsSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  unlinkSync,
  writeFileSync,
} = require("fs");
const { tmpdir } = require("os");
const { join } = require("path");
const { pipeline } = require("stream");
const { promisify } = require("util");
const { execFileSync } = require("child_process");

const streamPipeline = promisify(pipeline);
const GITHUB_REPO = "Loctree/aicx";
const VERSION = require("./package.json").version;

const BINARY_MAPPINGS = {
  "@loctree/aicx-darwin-arm64": {
    file: `aicx-v${VERSION}-aarch64-apple-darwin.zip`,
    bundleDir: `aicx-v${VERSION}-aarch64-apple-darwin`,
    archiveType: "zip",
  },
  "@loctree/aicx-darwin-x64": {
    file: `aicx-v${VERSION}-x86_64-apple-darwin.zip`,
    bundleDir: `aicx-v${VERSION}-x86_64-apple-darwin`,
    archiveType: "zip",
  },
  "@loctree/aicx-linux-x64-gnu": {
    file: `aicx-v${VERSION}-x86_64-unknown-linux-musl.tar.gz`,
    bundleDir: `aicx-v${VERSION}-x86_64-unknown-linux-musl`,
    archiveType: "tar.gz",
  },
  "@loctree/aicx-linux-x64-musl": {
    file: `aicx-v${VERSION}-x86_64-unknown-linux-musl.tar.gz`,
    bundleDir: `aicx-v${VERSION}-x86_64-unknown-linux-musl`,
    archiveType: "tar.gz",
  },
};

async function downloadFile(url, destPath) {
  return new Promise((resolve, reject) => {
    https.get(url, { headers: { "User-Agent": "aicx-npm-installer" } }, (response) => {
      if (response.statusCode === 301 || response.statusCode === 302) {
        return downloadFile(response.headers.location, destPath).then(resolve).catch(reject);
      }

      if (response.statusCode !== 200) {
        reject(new Error(`Failed to download: HTTP ${response.statusCode}`));
        return;
      }

      const chunks = [];
      response.on("data", (chunk) => chunks.push(chunk));
      response.on("end", () => {
        writeFileSync(destPath, Buffer.concat(chunks));
        resolve();
      });
      response.on("error", reject);
    }).on("error", reject);
  });
}

async function downloadText(url) {
  return new Promise((resolve, reject) => {
    https.get(url, { headers: { "User-Agent": "aicx-npm-installer" } }, (response) => {
      if (response.statusCode === 301 || response.statusCode === 302) {
        return downloadText(response.headers.location).then(resolve).catch(reject);
      }

      if (response.statusCode !== 200) {
        reject(new Error(`Failed to download: HTTP ${response.statusCode}`));
        return;
      }

      const chunks = [];
      response.on("data", (chunk) => chunks.push(chunk));
      response.on("end", () => resolve(Buffer.concat(chunks).toString("utf8")));
      response.on("error", reject);
    }).on("error", reject);
  });
}

function verifySha256(filePath, expectedDigest) {
  const digest = crypto.createHash("sha256").update(readFileSync(filePath)).digest("hex");
  if (digest !== expectedDigest) {
    throw new Error(`SHA256 mismatch: expected ${expectedDigest}, got ${digest}`);
  }
}

function extractArchive(archivePath, destDir, archiveType) {
  if (archiveType === "zip") {
    execFileSync("unzip", ["-q", archivePath, "-d", destDir], { stdio: "inherit" });
    return;
  }

  if (archiveType === "tar.gz") {
    execFileSync("tar", ["-xzf", archivePath, "-C", destDir], { stdio: "inherit" });
    return;
  }

  throw new Error(`Unsupported archive type: ${archiveType}`);
}

async function install() {
  const packageName = require("./package.json").name;
  const mapping = BINARY_MAPPINGS[packageName];

  if (!mapping) {
    console.error(`Unknown package: ${packageName}`);
    process.exit(1);
  }

  const targetAicx = join(__dirname, "aicx");
  const targetAicxMcp = join(__dirname, "aicx-mcp");
  if (existsSync(targetAicx) && existsSync(targetAicxMcp)) {
    console.log(`Binaries already exist at ${__dirname}`);
    return;
  }

  const baseUrl = `https://github.com/${GITHUB_REPO}/releases/download/v${VERSION}`;
  const archiveUrl = `${baseUrl}/${mapping.file}`;
  const checksumUrl = `${archiveUrl}.sha256`;
  const tempDir = mkdtempSync(join(tmpdir(), "aicx-npm-install-"));
  const archivePath = join(tempDir, mapping.file);

  console.log(`Downloading aicx release asset from ${archiveUrl}...`);

  try {
    await downloadFile(archiveUrl, archivePath);
    const checksumText = await downloadText(checksumUrl);
    const expectedDigest = checksumText.trim().split(/\s+/)[0];
    verifySha256(archivePath, expectedDigest);

    extractArchive(archivePath, tempDir, mapping.archiveType);

    const bundleDir = join(tempDir, mapping.bundleDir);
    copyFileSync(join(bundleDir, "aicx"), targetAicx);
    copyFileSync(join(bundleDir, "aicx-mcp"), targetAicxMcp);
    chmodSync(targetAicx, 0o755);
    chmodSync(targetAicxMcp, 0o755);

    unlinkSync(archivePath);
    rmSync(tempDir, { recursive: true, force: true });

    console.log(`Successfully installed aicx binaries to ${__dirname}`);
  } catch (error) {
    rmSync(tempDir, { recursive: true, force: true });
    console.error(`Failed to install aicx binaries: ${error.message}`);
    console.error(`Archive URL: ${archiveUrl}`);
    console.error(`Checksum URL: ${checksumUrl}`);
    process.exit(1);
  }
}

install();
