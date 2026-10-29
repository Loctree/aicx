const assert = require("node:assert/strict");
const { spawnSync } = require("node:child_process");
const { copyFileSync, mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } = require("node:fs");
const { tmpdir } = require("node:os");
const { join } = require("node:path");
const test = require("node:test");

const supported = [
  ["darwin", "arm64", "none", "darwin-arm64"],
  ["darwin", "x64", "none", "darwin-x64"],
  ["linux", "x64", "glibc", "linux-x64-gnu"],
  ["linux", "arm64", "glibc", "linux-arm64-gnu"],
  ["win32", "x64", "none", "win32-x64-gnu"],
];

// Exercise the real CommonJS module and bin entrypoints in a fresh process.
// Only host detection is simulated; resolution and filesystem checks are real.
function fixture(t, platform, arch, libc, packageKey) {
  const root = mkdtempSync(join(tmpdir(), "aicx-wrapper-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(join(root, "bin"));
  for (const file of ["index.js", "bin/aicx", "bin/aicx-mcp"]) {
    copyFileSync(join(__dirname, file), join(root, file));
  }
  const preload = join(root, "host.cjs");
  writeFileSync(preload, `
const childProcess = require("node:child_process");
const originalSpawnSync = childProcess.spawnSync;
Object.defineProperty(process, "platform", { value: ${JSON.stringify(platform)} });
Object.defineProperty(process, "arch", { value: ${JSON.stringify(arch)} });
process.report.getReport = () => ({ header: ${libc === "glibc" ? '{ glibcVersionRuntime: "2.36" }' : "{}"} });
childProcess.spawnSync = (command, args, options) => command === "ldd"
  ? { stdout: ${JSON.stringify(libc === "musl" ? "musl libc (aarch64)" : libc === "unknown" ? "" : "ldd (GNU libc) 2.36")}, stderr: "", status: 0 }
  : originalSpawnSync(command, args, options);
require("node:os").homedir = () => ${JSON.stringify(root)};
`);
  let packageRoot;
  if (packageKey) {
    packageRoot = join(root, "node_modules", "@loctree", `aicx-${packageKey}`);
    mkdirSync(join(packageRoot, "bin"), { recursive: true });
    writeFileSync(join(packageRoot, "package.json"), JSON.stringify({
      name: `@loctree/aicx-${packageKey}`, version: "0.15.0",
    }));
    const extension = platform === "win32" ? ".exe" : "";
    for (const binary of ["aicx", "aicx-mcp"]) {
      writeFileSync(join(packageRoot, "bin", binary + extension),
        '#!/usr/bin/env node\nprocess.stdout.write(JSON.stringify(process.argv.slice(2)));\n' +
        'if (process.env.AICX_TEST_SIGNAL) process.kill(process.pid, process.env.AICX_TEST_SIGNAL);\n' +
        'else process.exit(Number(process.env.AICX_TEST_EXIT || "0"));\n',
        { mode: 0o755 });
    }
  }
  return {
    root,
    packageRoot,
    run(args, env = {}) {
      return spawnSync(process.execPath, ["--require", preload, ...args], {
        cwd: root,
        env: { ...process.env, ...env },
        encoding: "utf8",
        timeout: 10000,
      });
    },
  };
}

for (const [platform, arch, libc, key] of supported) {
  test(`resolves ${key} to its prebuilt platform package`, (t) => {
    const host = fixture(t, platform, arch, libc, key);
    const result = host.run(["-e", 'process.stdout.write(require("./index.js").getPlatformPackageName())']);
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout, `@loctree/aicx-${key}`);
    for (const binary of ["aicx", "aicx-mcp"]) {
      const resolved = host.run(["-e", `process.stdout.write(require("./index.js").getBinaryPath(${JSON.stringify(binary)}))`]);
      assert.equal(resolved.status, 0, resolved.stderr);
      assert.equal(resolved.stdout, realpathSync(join(host.packageRoot, "bin", binary + (platform === "win32" ? ".exe" : ""))));
    }
  });
}

function assertLoudRefusal(result, platform, arch, libc) {
  assert.equal(result.error, undefined);
  assert.notEqual(result.status, 0, "wrapper silently succeeded");
  assert.equal(result.stdout, "", "diagnostics belong on stderr");
  assert.ok(result.stderr.includes(platform), result.stderr);
  assert.ok(result.stderr.includes(arch), result.stderr);
  assert.match(result.stderr, /libc=/i);
  if (libc !== "none") assert.ok(result.stderr.includes(libc), result.stderr);
  assert.match(result.stderr, /supported/i);
  for (const [, , , key] of supported) assert.ok(result.stderr.includes(key), result.stderr);
  assert.ok(result.stderr.includes("https://github.com/Loctree/aicx/issues"), result.stderr);
}

for (const binary of ["aicx", "aicx-mcp"]) {
  for (const [name, platform, arch, libc, packageKey] of [
    ["unsupported OS", "freebsd", "x64", "none", null],
    ["unsupported architecture", "linux", "s390x", "glibc", null],
    ["musl", "linux", "arm64", "musl", null],
    ["unknown libc", "linux", "arm64", "unknown", null],
    ["missing optional package", "linux", "arm64", "glibc", null],
    ["missing binary payload", "linux", "arm64", "glibc", "linux-arm64-gnu"],
  ]) {
    test(`${binary} loudly refuses ${name}`, (t) => {
      const host = fixture(t, platform, arch, libc, packageKey);
      if (packageKey) rmSync(join(host.packageRoot, "bin", binary));
      const result = host.run([join("bin", binary), "--version"]);
      assertLoudRefusal(result, platform, arch, libc);
    });
  }

  if (process.platform !== "win32") {
    test(`${binary} forwards arguments to the resolved native executable`, (t) => {
      const host = fixture(t, "linux", "arm64", "glibc", "linux-arm64-gnu");
      const args = ["--version", "argument with spaces", "--literal=$HOME"];
      const result = host.run([join("bin", binary), ...args]);
      assert.equal(result.status, 0, result.stderr);
      assert.deepEqual(JSON.parse(result.stdout), args);
      assert.equal(result.stderr, "");
    });

    test(`${binary} preserves a native nonzero exit`, (t) => {
      const host = fixture(t, "linux", "arm64", "glibc", "linux-arm64-gnu");
      const result = host.run([join("bin", binary)], { AICX_TEST_EXIT: "23" });
      assert.equal(result.status, 23, result.stderr);
    });

    test(`${binary} reports native signal death with a nonzero exit`, (t) => {
      const host = fixture(t, "linux", "arm64", "glibc", "linux-arm64-gnu");
      const result = host.run([join("bin", binary)], { AICX_TEST_SIGNAL: "SIGTERM" });
      assert.notEqual(result.status, 0);
      assert.match(result.stderr, /SIGTERM/);
    });
  }
}
