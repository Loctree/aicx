"use strict";

const assert = require("node:assert/strict");
const { spawnSync } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { test } = require("node:test");

const WRAPPER_ROOT = path.resolve(__dirname, "..");
const PRELOAD = path.join(__dirname, "simulate-host.cjs");
const { PLATFORM_PACKAGES, platformKeyFor } = require(path.join(WRAPPER_ROOT, "index.js"));

const SUPPORTED_KEYS = [
  "darwin-arm64",
  "darwin-x64",
  "linux-arm64-gnu",
  "linux-x64-gnu",
  "win32-x64-gnu",
];

// Copy the wrapper into an isolated tree so module resolution can neither find
// a real platform package from the repo nor leak into one.
function stageWrapper() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "aicx-wrapper-test-"));
  const wrapper = path.join(root, "node_modules", "@loctree", "aicx");
  fs.mkdirSync(path.join(wrapper, "bin"), { recursive: true });
  fs.copyFileSync(path.join(WRAPPER_ROOT, "index.js"), path.join(wrapper, "index.js"));
  for (const bin of ["aicx", "aicx-mcp"]) {
    fs.copyFileSync(path.join(WRAPPER_ROOT, "bin", bin), path.join(wrapper, "bin", bin));
  }
  return { root, wrapper };
}

function runWrapper(wrapper, host, args = ["--version"], bin = "aicx") {
  return spawnSync(process.execPath, ["-r", PRELOAD, path.join(wrapper, "bin", bin), ...args], {
    encoding: "utf8",
    env: {
      ...process.env,
      AICX_TEST_PLATFORM: host.platform,
      AICX_TEST_ARCH: host.arch,
      AICX_TEST_LIBC: host.libc || "",
    },
  });
}

test("platform map covers exactly the published platform packages", () => {
  assert.deepEqual(Object.keys(PLATFORM_PACKAGES).sort(), SUPPORTED_KEYS);
  for (const key of SUPPORTED_KEYS) {
    assert.equal(PLATFORM_PACKAGES[key].name, `@loctree/aicx-${key}`);
  }
});

test("platformKeyFor maps hosts to platform keys", () => {
  const cases = [
    [{ platform: "darwin", arch: "arm64", libc: null }, "darwin-arm64"],
    [{ platform: "darwin", arch: "x64", libc: null }, "darwin-x64"],
    [{ platform: "linux", arch: "x64", libc: "glibc" }, "linux-x64-gnu"],
    [{ platform: "linux", arch: "arm64", libc: "glibc" }, "linux-arm64-gnu"],
    [{ platform: "win32", arch: "x64", libc: null }, "win32-x64-gnu"],
    [{ platform: "linux", arch: "arm64", libc: "musl" }, null],
    [{ platform: "linux", arch: "x64", libc: "musl" }, null],
    [{ platform: "linux", arch: "arm", libc: "glibc" }, null],
    [{ platform: "linux", arch: "riscv64", libc: "glibc" }, null],
    [{ platform: "win32", arch: "arm64", libc: null }, null],
    [{ platform: "freebsd", arch: "x64", libc: null }, null],
  ];
  for (const [host, expected] of cases) {
    assert.equal(platformKeyFor(host), expected, JSON.stringify(host));
  }
});

const UNSUPPORTED_HOSTS = [
  { platform: "linux", arch: "arm64", libc: "musl" },
  { platform: "linux", arch: "riscv64", libc: "glibc" },
  { platform: "win32", arch: "arm64", libc: null },
  { platform: "freebsd", arch: "x64", libc: null },
];

for (const bin of ["aicx", "aicx-mcp"]) {
  for (const host of UNSUPPORTED_HOSTS) {
    test(`${bin} refuses loudly on ${host.platform}/${host.arch}${host.libc ? `/${host.libc}` : ""}`, () => {
      const { root, wrapper } = stageWrapper();
      try {
        const result = runWrapper(wrapper, host, ["--version"], bin);
        assert.notEqual(result.status, 0, "exit code must be non-zero");
        assert.equal(result.stdout, "", "refusal must not write to stdout");
        assert.match(result.stderr, /No prebuilt aicx binary for this platform/);
        assert.ok(result.stderr.includes(`process.platform: ${host.platform}`), result.stderr);
        assert.ok(result.stderr.includes(`process.arch:     ${host.arch}`), result.stderr);
        if (host.libc) assert.ok(result.stderr.includes(`libc:             ${host.libc}`), result.stderr);
        for (const key of SUPPORTED_KEYS) {
          assert.ok(result.stderr.includes(key), `stderr must list ${key}:\n${result.stderr}`);
        }
        assert.ok(result.stderr.includes("https://github.com/Loctree/aicx/issues"), result.stderr);
        assert.ok(!/\n\s+at /.test(result.stderr), `no stack trace expected:\n${result.stderr}`);
      } finally {
        fs.rmSync(root, { recursive: true, force: true });
      }
    });
  }
}

test("missing platform package names the package and the usual causes", () => {
  const { root, wrapper } = stageWrapper();
  try {
    const result = runWrapper(wrapper, { platform: "linux", arch: "arm64", libc: "glibc" });
    assert.notEqual(result.status, 0);
    assert.equal(result.stdout, "");
    assert.ok(result.stderr.includes("@loctree/aicx-linux-arm64-gnu is not installed"), result.stderr);
    assert.ok(result.stderr.includes("process.arch:     arm64"), result.stderr);
    assert.ok(result.stderr.includes("optionalDependencies"), result.stderr);
    assert.ok(result.stderr.includes("https://github.com/Loctree/aicx/issues"), result.stderr);
    assert.ok(!/\n\s+at /.test(result.stderr), `no stack trace expected:\n${result.stderr}`);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test(
  "supported host with its platform package runs the packaged binary",
  { skip: process.platform === "win32" },
  () => {
    const { root, wrapper } = stageWrapper();
    try {
      const packageRoot = path.join(root, "node_modules", "@loctree", "aicx-linux-arm64-gnu");
      fs.mkdirSync(path.join(packageRoot, "bin"), { recursive: true });
      fs.writeFileSync(
        path.join(packageRoot, "package.json"),
        `${JSON.stringify({ name: "@loctree/aicx-linux-arm64-gnu", version: "0.0.0" })}\n`,
      );
      for (const bin of ["aicx", "aicx-mcp"]) {
        fs.writeFileSync(
          path.join(packageRoot, "bin", bin),
          `#!/bin/sh\necho "${bin} fake 0.0.0 $*"\n`,
          { mode: 0o755 },
        );
      }
      const host = { platform: "linux", arch: "arm64", libc: "glibc" };
      const aicx = runWrapper(wrapper, host, ["--version"], "aicx");
      assert.equal(aicx.status, 0, aicx.stderr);
      assert.equal(aicx.stdout.trim(), "aicx fake 0.0.0 --version");
      const mcp = runWrapper(wrapper, host, ["--version"], "aicx-mcp");
      assert.equal(mcp.status, 0, mcp.stderr);
      assert.equal(mcp.stdout.trim(), "aicx-mcp fake 0.0.0 --version");
    } finally {
      fs.rmSync(root, { recursive: true, force: true });
    }
  },
);
