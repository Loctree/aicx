'use strict';

const assert = require('assert');
const { spawnSync } = require('child_process');
const path = require('path');
const test = require('node:test');

const root = path.resolve(__dirname, '..');
function run(platform, arch, libc, expression) {
  const setup = `
    Object.defineProperty(process, 'platform', { value: ${JSON.stringify(platform)} });
    Object.defineProperty(process, 'arch', { value: ${JSON.stringify(arch)} });
    require('child_process').spawnSync = () => ({ stdout: ${JSON.stringify(libc)}, stderr: '' });
    ${expression}
  `;
  return spawnSync(process.execPath, ['-e', setup], { cwd: root, encoding: 'utf8' });
}

for (const [platform, arch, libc, expected] of [
  ['darwin', 'arm64', '', '@loctree/aicx-darwin-arm64'],
  ['darwin', 'x64', '', '@loctree/aicx-darwin-x64'],
  ['linux', 'arm64', 'glibc', '@loctree/aicx-linux-arm64-gnu'],
  ['linux', 'x64', 'glibc', '@loctree/aicx-linux-x64-gnu'],
  ['win32', 'x64', '', '@loctree/aicx-win32-x64-gnu'],
]) {
  test(`selects ${expected}`, () => {
    const result = run(platform, arch, libc, "console.log(require('./index.js').getPlatformPackageName())");
    assert.strictEqual(result.status, 0, result.stderr);
    assert.strictEqual(result.stdout.trim(), expected);
  });
}

for (const binary of ['aicx', 'aicx-mcp']) {
  test(`${binary} loudly rejects Linux musl`, () => {
    const result = run('linux', 'arm64', 'musl libc', `require('./bin/${binary}')`);
    assert.notStrictEqual(result.status, 0);
    assert.match(result.stderr, /platform=linux, arch=arm64, libc=musl/);
    assert.match(result.stderr, /Supported platforms:.*linux-arm64-gnu.*darwin-x64|Supported platforms:.*darwin-x64.*linux-arm64-gnu/);
    assert.match(result.stderr, /https:\/\/github.com\/Loctree\/aicx\/issues/);
  });
}

test('missing optional package reports host and supported platforms', () => {
  const result = run('linux', 'arm64', 'glibc', `
    const Module = require('module');
    const resolve = Module._resolveFilename;
    Module._resolveFilename = function (request, ...args) {
      if (request === '@loctree/aicx-linux-arm64-gnu/package.json') {
        const error = new Error('simulated missing optional dependency');
        error.code = 'MODULE_NOT_FOUND';
        throw error;
      }
      return resolve.call(this, request, ...args);
    };
    require('./bin/aicx');
  `);
  assert.notStrictEqual(result.status, 0);
  assert.match(result.stderr, /@loctree\/aicx-linux-arm64-gnu is not installed/);
  assert.match(result.stderr, /platform=linux, arch=arm64, libc=gnu/);
  assert.match(result.stderr, /Supported platforms:/);
  assert.match(result.stderr, /https:\/\/github.com\/Loctree\/aicx\/issues/);
});
