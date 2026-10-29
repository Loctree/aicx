"use strict";

// Preload for wrapper.test.js: make the wrapper believe it runs on another host
// without touching production code. Pairs with `node -r <this file> bin/aicx`.
const platform = process.env.AICX_TEST_PLATFORM;
const arch = process.env.AICX_TEST_ARCH;
const libc = process.env.AICX_TEST_LIBC;

if (platform) Object.defineProperty(process, "platform", { value: platform });
if (arch) Object.defineProperty(process, "arch", { value: arch });

if (libc) {
  const childProcess = require("node:child_process");
  const realSpawnSync = childProcess.spawnSync;
  childProcess.spawnSync = (command, args, options) => {
    if (command === "ldd") {
      const banner = libc === "musl"
        ? "musl libc (aarch64)\nVersion 1.2.5\n"
        : "ldd (Debian GLIBC 2.36-9+deb12u10) 2.36\n";
      return { status: libc === "musl" ? 1 : 0, stdout: "", stderr: banner };
    }
    return realSpawnSync(command, args, options);
  };
}
