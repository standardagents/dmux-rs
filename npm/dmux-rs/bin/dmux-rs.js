#!/usr/bin/env node
// Resolve and exec the platform-native dmux-rs binary (esbuild/biome pattern):
// the real binary ships in a platform-specific optional dependency.
const { spawnSync } = require("node:child_process");
const path = require("node:path");

const PLATFORM_PACKAGES = {
  "darwin arm64": "@dmux/darwin-arm64",
  "darwin x64": "@dmux/darwin-x64",
  "linux x64": "@dmux/linux-x64-gnu",
  "linux arm64": "@dmux/linux-arm64-gnu",
};

function resolveBinary() {
  const key = `${process.platform} ${process.arch}`;
  const pkg = PLATFORM_PACKAGES[key];
  if (!pkg) {
    console.error(`dmux-rs: unsupported platform ${key}`);
    process.exit(1);
  }
  try {
    return require.resolve(`${pkg}/bin/dmux-rs`);
  } catch {
    console.error(
      `dmux-rs: platform package ${pkg} is not installed.\n` +
        "This usually means npm was run with --omit=optional or the install was interrupted.\n" +
        "Try: npm install -g dmux-rs --force"
    );
    process.exit(1);
  }
}

const binary = resolveBinary();
const result = spawnSync(binary, process.argv.slice(2), { stdio: "inherit" });
if (result.error) {
  console.error(`dmux-rs: failed to launch ${path.basename(binary)}: ${result.error.message}`);
  process.exit(1);
}
process.exit(result.status ?? 1);
