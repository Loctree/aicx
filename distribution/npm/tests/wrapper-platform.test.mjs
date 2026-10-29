// Platform resolution contract for the script-free @loctree/aicx wrapper.
//
//   node --test distribution/npm/tests/wrapper-platform.test.mjs
//
// The subprocess cases run the real bin shims from a throwaway copy of the
// wrapper, impersonating other hosts through fake-host.cjs, so a refusal is
// checked the way a user meets it: exit status, stderr, no stack trace.

import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "node:child_process";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const WRAPPER_ROOT = path.join(HERE, "..", "aicx");
const FAKE_HOST = path.join(HERE, "fake-host.cjs");
const require = createRequire(import.meta.url);
const wrapper = require(path.join(WRAPPER_ROOT, "index.js"));
const manifest = JSON.parse(fs.readFileSync(path.join(WRAPPER_ROOT, "package.json"), "utf8"));
const SUPPORTED_KEYS = Object.keys(wrapper.PLATFORM_PACKAGES);
const ISSUES_URL = "https://github.com/Loctree/aicx/issues";

function makeWrapperCopy() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "aicx-wrapper-test-"));
  const copy = path.join(root, "node_modules", "@loctree", "aicx");
  fs.mkdirSync(path.join(copy, "bin"), { recursive: true });
  for (const file of ["index.js", "package.json", "bin/aicx", "bin/aicx-mcp"]) {
    fs.copyFileSync(path.join(WRAPPER_ROOT, file), path.join(copy, file));
  }
  return { root, copy };
}

function runShim(copy, binary, host, args = ["--version"]) {
  return spawnSync(process.execPath, ["-r", FAKE_HOST, path.join(copy, "bin", binary), ...args], {
    encoding: "utf8",
    env: {
      ...process.env,
      AICX_FAKE_PLATFORM: host.platform,
      AICX_FAKE_ARCH: host.arch,
      AICX_FAKE_LIBC: host.libc || "",
    },
  });
}

test("host descriptions map to the published package keys", () => {
  const cases = [
    [{ platform: "darwin", arch: "arm64", libc: null }, "darwin-arm64"],
    [{ platform: "darwin", arch: "x64", libc: null }, "darwin-x64"],
    [{ platform: "linux", arch: "arm64", libc: "glibc" }, "linux-arm64-gnu"],
    [{ platform: "linux", arch: "aarch64", libc: "glibc" }, "linux-arm64-gnu"],
    [{ platform: "linux", arch: "x64", libc: "glibc" }, "linux-x64-gnu"],
    [{ platform: "linux", arch: "arm64", libc: "musl" }, "linux-arm64-musl"],
    [{ platform: "win32", arch: "x64", libc: null }, "win32-x64-gnu"],
    [{ platform: "win32", arch: "arm64", libc: null }, "win32-arm64-gnu"],
    [{ platform: "freebsd", arch: "x64", libc: null }, "freebsd-x64"],
  ];
  for (const [host, key] of cases) {
    assert.equal(wrapper.platformKeyFor(host), key, JSON.stringify(host));
  }
  assert.deepEqual(SUPPORTED_KEYS.sort(), [
    "darwin-arm64",
    "darwin-x64",
    "linux-arm64-gnu",
    "linux-x64-gnu",
    "win32-x64-gnu",
  ]);
});

test("every supported key has a fenced platform package and an optional dependency", () => {
  assert.deepEqual(
    Object.keys(manifest.optionalDependencies).sort(),
    Object.values(wrapper.PLATFORM_PACKAGES).map((pkg) => pkg.name).sort(),
  );
  for (const [key, pkg] of Object.entries(wrapper.PLATFORM_PACKAGES)) {
    const platformManifest = JSON.parse(
      fs.readFileSync(path.join(WRAPPER_ROOT, "platform-packages", key, "package.json"), "utf8"),
    );
    assert.equal(platformManifest.name, pkg.name);
    assert.equal(platformManifest.version, manifest.version);
    assert.equal(manifest.optionalDependencies[pkg.name], manifest.version);
    // npm installs a platform package only where its os/cpu/libc fence
    // matches; that fence must describe the same host the wrapper resolves.
    const fencedHost = {
      platform: platformManifest.os[0],
      arch: platformManifest.cpu[0],
      libc: platformManifest.libc ? platformManifest.libc[0] : null,
    };
    assert.equal(wrapper.platformKeyFor(fencedHost), key, `${key} fence ${JSON.stringify(fencedHost)}`);
  }
});

test("unsupported hosts raise a platform error naming the host and the supported set", () => {
  const host = { platform: "linux", arch: "arm64", libc: "musl" };
  assert.throws(
    () => wrapper.getPlatformPackage(host),
    (error) => {
      assert.ok(error instanceof wrapper.AicxPlatformError);
      assert.notEqual(error.exitCode, 0);
      assert.match(error.message, /platform: linux/);
      assert.match(error.message, /arch: {5}arm64/);
      assert.match(error.message, /libc: {5}musl/);
      assert.match(error.message, /key: {6}linux-arm64-musl/);
      for (const key of SUPPORTED_KEYS) assert.ok(error.message.includes(key), key);
      assert.ok(error.message.includes(ISSUES_URL));
      return true;
    },
  );
  assert.equal(
    wrapper.getPlatformPackage({ platform: "linux", arch: "arm64", libc: "glibc" }).name,
    "@loctree/aicx-linux-arm64-gnu",
  );
  assert.equal(wrapper.getPlatformPackage({ platform: "darwin", arch: "x64" }).name, "@loctree/aicx-darwin-x64");
});

for (const host of [
  { platform: "linux", arch: "arm64", libc: "musl" },
  { platform: "linux", arch: "x64", libc: "musl" },
  { platform: "win32", arch: "arm64" },
  { platform: "freebsd", arch: "x64" },
]) {
  test(`bin shims refuse ${host.platform}/${host.arch}${host.libc ? `/${host.libc}` : ""} loudly`, (t) => {
    const { root, copy } = makeWrapperCopy();
    t.after(() => fs.rmSync(root, { recursive: true, force: true }));
    for (const binary of ["aicx", "aicx-mcp"]) {
      const result = runShim(copy, binary, host);
      assert.equal(result.status, 1, `${binary} exit status; stderr=${result.stderr}`);
      assert.equal(result.stdout, "");
      assert.match(result.stderr, /no prebuilt aicx binary is published for this platform/);
      assert.ok(result.stderr.includes(`platform: ${host.platform}`), result.stderr);
      assert.ok(result.stderr.includes(`arch:     ${host.arch}`), result.stderr);
      assert.ok(result.stderr.includes(`libc:     ${host.libc || "n/a"}`), result.stderr);
      for (const key of SUPPORTED_KEYS) assert.ok(result.stderr.includes(key), `${key} in ${result.stderr}`);
      assert.ok(result.stderr.includes(ISSUES_URL));
      assert.doesNotMatch(result.stderr, /^\s+at /m, "refusal must not print a stack trace");
    }
  });
}

test("bin shims explain a skipped optional platform package", (t) => {
  const { root, copy } = makeWrapperCopy();
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const result = runShim(copy, "aicx", { platform: "linux", arch: "arm64", libc: "glibc" });
  assert.equal(result.status, 1, result.stderr);
  assert.match(result.stderr, /@loctree\/aicx-linux-arm64-gnu is not installed or cannot be resolved/);
  assert.match(result.stderr, /libc: {5}glibc/);
  assert.match(result.stderr, /npm install --include=optional @loctree\/aicx/);
  assert.doesNotMatch(result.stderr, /^\s+at /m);
});

test("bin shims exec the resolved platform binary", { skip: process.platform === "win32" }, (t) => {
  const { root, copy } = makeWrapperCopy();
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const platformRoot = path.join(root, "node_modules", "@loctree", "aicx-linux-arm64-gnu");
  fs.mkdirSync(path.join(platformRoot, "bin"), { recursive: true });
  fs.writeFileSync(
    path.join(platformRoot, "package.json"),
    `${JSON.stringify({ name: "@loctree/aicx-linux-arm64-gnu", version: manifest.version })}\n`,
  );
  for (const binary of ["aicx", "aicx-mcp"]) {
    const binaryPath = path.join(platformRoot, "bin", binary);
    fs.writeFileSync(binaryPath, `#!/bin/sh\necho "${binary} ${manifest.version} $*"\n`);
    fs.chmodSync(binaryPath, 0o755);
    const result = runShim(copy, binary, { platform: "linux", arch: "arm64", libc: "glibc" });
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout.trim(), `${binary} ${manifest.version} --version`);
  }
});
