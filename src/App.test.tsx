import { cleanup, fireEvent, render, screen } from "@testing-library/react"
import { afterEach, describe, expect, it } from "vitest"
import App from "./App"

afterEach(cleanup)

describe("Recall Memory shell", () => {
  it("shows first-use recovery actions when the Tauri backend is unavailable", async () => {
    render(<App />)

    expect(await screen.findByText("首次使用检查")).toBeInTheDocument()
    expect(screen.getByRole("button", { name: "授权并检测" })).toBeEnabled()
    expect(screen.getByRole("button", { name: "配置模型" })).toBeEnabled()
    expect(screen.getByText("请在 Tauri 桌面 App 中运行")).toBeInTheDocument()
  })

  it("exposes agent mode and configurable Ollama fallback settings", async () => {
    render(<App />)
    fireEvent.click(await screen.findByRole("button", { name: "配置模型" }))

    expect(await screen.findByText("备用 Ollama")).toBeInTheDocument()
    expect(screen.getByDisplayValue("http://127.0.0.1:11434/v1")).toBeInTheDocument()
    expect(screen.getByDisplayValue("qwen3:8b")).toBeInTheDocument()
    fireEvent.click(screen.getByRole("button", { name: "验证" }))
    expect(screen.getByRole("button", { name: "验证" })).toHaveClass("selected")
  })
})
