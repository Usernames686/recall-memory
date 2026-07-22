import { copyFileSync, mkdirSync } from "node:fs"
import { execFileSync } from "node:child_process"
import { dirname, resolve } from "node:path"

const root = resolve(import.meta.dirname, "..")
const tauri = resolve(root, "src-tauri")
const profile = process.env.TAURI_DEBUG === "true" ? "debug" : "release"
const args = ["build", "--bin", "evolution-mcp"]
if (profile === "release") args.push("--release")

execFileSync("cargo", args, { cwd: tauri, stdio: "inherit" })
const source = resolve(tauri, "target", profile, "evolution-mcp")
const target = resolve(tauri, "binaries", "evolution-mcp-aarch64-apple-darwin")
mkdirSync(dirname(target), { recursive: true })
copyFileSync(source, target)
