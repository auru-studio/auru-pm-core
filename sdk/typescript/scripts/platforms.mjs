/**
 * The platforms a native addon is published for.
 *
 * Each becomes its own npm package carrying one `.node` file, listed in the
 * main package's `optionalDependencies`. npm installs only the entry whose
 * `os`/`cpu`/`libc` match, so a macOS user never downloads a Linux binary —
 * which is the whole reason not to ship them all in one package.
 */
export const PLATFORMS = [
  { id: "darwin-arm64", target: "aarch64-apple-darwin", os: "darwin", cpu: "arm64", artifact: "libauru_pm_ffi.dylib" },
  { id: "darwin-x64", target: "x86_64-apple-darwin", os: "darwin", cpu: "x64", artifact: "libauru_pm_ffi.dylib" },
  { id: "linux-x64-gnu", target: "x86_64-unknown-linux-gnu", os: "linux", cpu: "x64", libc: "glibc", artifact: "libauru_pm_ffi.so" },
  { id: "linux-arm64-gnu", target: "aarch64-unknown-linux-gnu", os: "linux", cpu: "arm64", libc: "glibc", artifact: "libauru_pm_ffi.so" },
  { id: "linux-x64-musl", target: "x86_64-unknown-linux-musl", os: "linux", cpu: "x64", libc: "musl", artifact: "libauru_pm_ffi.so" },
  { id: "win32-x64-msvc", target: "x86_64-pc-windows-msvc", os: "win32", cpu: "x64", artifact: "auru_pm_ffi.dll" },
];

/** The package name carrying the addon for one platform. */
export function packageName(id) {
  return `@auru/pm-${id}`;
}
