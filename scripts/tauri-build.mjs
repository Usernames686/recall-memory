import { execFileSync } from "node:child_process"
import { resolve } from "node:path"

const root = resolve(import.meta.dirname, "..")
const tauri = resolve(root, "node_modules", ".bin", "tauri")
const env = { ...process.env }

// Tauri otherwise leaves only linker signatures on local macOS bundles, which
// do not seal Info.plist and bundled sidecars. CI supplies a Developer ID.
if (process.platform === "darwin" && !env.APPLE_SIGNING_IDENTITY) {
  env.APPLE_SIGNING_IDENTITY = "-"
}

execFileSync(tauri, ["build", ...process.argv.slice(2)], {
  cwd: root,
  env,
  stdio: "inherit",
})
