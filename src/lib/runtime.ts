import { invoke as tauriInvoke } from "@tauri-apps/api/core"

export const isTauri = () =>
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window

export async function invoke<T>(command: string, args?: Record<string, unknown>) {
  if (!isTauri()) {
    throw new Error("请在 Tauri 桌面 App 中运行")
  }
  return tauriInvoke<T>(command, args)
}
