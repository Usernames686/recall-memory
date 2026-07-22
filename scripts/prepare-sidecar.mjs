import { copyFileSync, existsSync, mkdirSync } from "node:fs"
import { execFileSync } from "node:child_process"
import { dirname, resolve } from "node:path"

const root = resolve(import.meta.dirname, "..")
const tauri = resolve(root, "src-tauri")
const profile = process.env.TAURI_DEBUG === "true" ? "debug" : "release"
const targetIndex = process.argv.indexOf("--target")
const requestedTarget = targetIndex >= 0 ? process.argv[targetIndex + 1] : undefined
const hostTarget = process.arch === "arm64" ? "aarch64-apple-darwin" : "x86_64-apple-darwin"
const target = requestedTarget || hostTarget
const binaries = resolve(tauri, "binaries")
mkdirSync(binaries, { recursive: true })

function buildSidecar(triple) {
  const expected = resolve(binaries, `evolution-mcp-${triple}`)
  if (!existsSync(expected)) {
    const hostSidecar = resolve(binaries, `evolution-mcp-${hostTarget}`)
    if (!existsSync(hostSidecar)) {
      throw new Error(`Build the host sidecar first: ${hostSidecar}`)
    }
    // Tauri's package build script requires the target sidecar path to exist
    // before Cargo can compile the sidecar for that target.
    copyFileSync(hostSidecar, expected)
  }
  const args = ["build", "--bin", "evolution-mcp", "--target", triple]
  if (profile === "release") args.push("--release")
  execFileSync("cargo", args, { cwd: tauri, stdio: "inherit" })
  const built = resolve(tauri, "target", triple, profile, "evolution-mcp")
  copyFileSync(built, expected)
  return expected
}

if (target === "universal-apple-darwin") {
  const arm = buildSidecar("aarch64-apple-darwin")
  const intel = buildSidecar("x86_64-apple-darwin")
  const output = resolve(binaries, "evolution-mcp-universal-apple-darwin")
  execFileSync("lipo", ["-create", "-output", output, arm, intel], { stdio: "inherit" })
  const tauriOutput = resolve(tauri, "target", "universal-apple-darwin", profile, "evolution-mcp")
  mkdirSync(dirname(tauriOutput), { recursive: true })
  copyFileSync(output, tauriOutput)
} else {
  buildSidecar(target)
}
