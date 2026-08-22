import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    include: ["test/**/*.test.ts"],
    // The live-server tests start a real auru-pm-server and push commits
    // through it; that is slower than a unit test but it is the only way to
    // know the transport actually speaks the protocol.
    testTimeout: 30_000,
  },
});
