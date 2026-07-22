import { expect, test } from "@playwright/test"

test("first-use flow and agent settings remain usable", async ({ page }) => {
  await page.goto("/")
  await expect(page.getByText("首次使用检查")).toBeVisible()
  await page.getByRole("button", { name: "配置模型" }).click()
  await expect(page.getByText("备用 Ollama")).toBeVisible()
  await expect(page.locator('input[value="qwen3:8b"]')).toBeVisible()
  await page.getByLabel("设置分类").getByRole("button", { name: "数据源" }).click()
  await expect(page.getByText("历史回看范围")).toBeVisible()
  await expect(page.getByRole("button", { name: "30 天" })).toBeVisible()
  await expect(page.getByLabel("Codex 根目录")).toHaveValue("~/.codex")
  await expect(page.getByLabel("Claude Code 根目录")).toHaveValue("~/.claude")
})
