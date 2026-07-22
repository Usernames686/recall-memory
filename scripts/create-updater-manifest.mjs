import { existsSync, readdirSync, readFileSync, writeFileSync } from "node:fs"
import { resolve } from "node:path"

const root = resolve(import.meta.dirname, "..")
const universalArtifactDirectory = resolve(
  root,
  "src-tauri",
  "target",
  "universal-apple-darwin",
  "release",
  "bundle",
  "macos",
)
const hostArtifactDirectory = resolve(root, "src-tauri", "target", "release", "bundle", "macos")
const hasArchive = (directory) => existsSync(directory)
  && readdirSync(directory).some((name) => name.endsWith(".tar.gz") && !name.endsWith(".sig"))
const artifactDirectory = process.env.TAURI_UPDATER_ARTIFACT_DIR
  ? resolve(root, process.env.TAURI_UPDATER_ARTIFACT_DIR)
  : hasArchive(universalArtifactDirectory)
    ? universalArtifactDirectory
    : hostArtifactDirectory
const config = JSON.parse(readFileSync(resolve(root, "src-tauri", "tauri.conf.json"), "utf8"))
const archive = readdirSync(artifactDirectory).find((name) => name.endsWith(".tar.gz") && !name.endsWith(".sig"))
if (!archive) throw new Error(`No macOS updater archive found in ${artifactDirectory}`)

const signature = readFileSync(resolve(artifactDirectory, `${archive}.sig`), "utf8").trim()
if (!signature) throw new Error(`Updater signature is empty for ${archive}`)

const repository = process.env.GITHUB_REPOSITORY || "Usernames686/recall-memory"
const server = process.env.GITHUB_SERVER_URL || "https://github.com"
const configuredTag = process.env.RECALL_RELEASE_TAG || process.env.GITHUB_REF_NAME || `v${config.version}`
if (!/^v\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/.test(configuredTag)) {
  throw new Error(`Updater manifest requires a release tag such as v${config.version}; received ${configuredTag}`)
}
const tag = configuredTag
const url = `${server}/${repository}/releases/download/${tag}/${encodeURIComponent(archive)}`
const manifest = {
  version: config.version,
  notes: "Recall Memory signed macOS update",
  pub_date: new Date().toISOString(),
  platforms: {
    "darwin-aarch64": { signature, url },
    "darwin-x86_64": { signature, url },
  },
}

writeFileSync(resolve(root, "latest.json"), `${JSON.stringify(manifest, null, 2)}\n`, { mode: 0o600 })
console.log(`Wrote latest.json for ${archive}`)
