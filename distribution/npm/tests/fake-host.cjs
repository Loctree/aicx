// Preloaded with `node -r` so wrapper tests can impersonate another host
// without a test hook in the shipped wrapper. Runs before index.js loads.
const { AICX_FAKE_PLATFORM, AICX_FAKE_ARCH, AICX_FAKE_LIBC } = process.env;

if (AICX_FAKE_PLATFORM) Object.defineProperty(process, "platform", { value: AICX_FAKE_PLATFORM });
if (AICX_FAKE_ARCH) Object.defineProperty(process, "arch", { value: AICX_FAKE_ARCH });
if (AICX_FAKE_LIBC) {
  const glibcVersionRuntime = AICX_FAKE_LIBC === "glibc" ? "2.36" : undefined;
  Object.defineProperty(process.report, "getReport", {
    value: () => ({ header: { glibcVersionRuntime } }),
  });
}
