import { execFileSync } from "node:child_process"
import { readFileSync } from "node:fs"
import { resolve } from "node:path"

const root = resolve(import.meta.dirname, "..")
const tauri = resolve(root, "node_modules", ".bin", "tauri")
const releaseConfig = resolve(root, "src-tauri", "tauri.release.conf.json")
const env = { ...process.env }

// Tauri otherwise leaves only linker signatures on local macOS bundles, which
// do not seal Info.plist and bundled sidecars. CI supplies a Developer ID.
if (process.platform === "darwin" && !env.APPLE_SIGNING_IDENTITY) {
  env.APPLE_SIGNING_IDENTITY = "-"
}

if (!env.TAURI_SIGNING_PRIVATE_KEY && env.TAURI_SIGNING_PRIVATE_KEY_PATH) {
  env.TAURI_SIGNING_PRIVATE_KEY = readFileSync(env.TAURI_SIGNING_PRIVATE_KEY_PATH, "utf8")
}

const args = ["build", ...process.argv.slice(2)]
if (env.TAURI_SIGNING_PRIVATE_KEY || env.TAURI_SIGNING_PRIVATE_KEY_PATH) {
  args.push("--config", releaseConfig)
}

execFileSync(tauri, args, {
  cwd: root,
  env,
  stdio: "inherit",
})
