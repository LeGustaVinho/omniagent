import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { copyFile, mkdtemp, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { remote } from "webdriverio";

const identifier = "io.github.legustavinho.omniagent.desktop-tests";
const port = Number(process.env.OMNIAGENT_WEBDRIVER_PORT ?? 4444);

async function waitForDriver(driver) {
  for (let attempt = 0; attempt < 60; attempt++) {
    if (driver.exitCode !== null) throw new Error("tauri-driver exited before opening a session");
    try {
      const response = await fetch(`http://127.0.0.1:${port}/status`, { signal: AbortSignal.timeout(500) });
      if (response.ok) return;
    } catch {
      // Tauri WebDriver and its Edge child need time to bind.
    }
    await delay(200);
  }
  throw new Error("tauri-driver did not become ready");
}

function git(cwd, ...args) {
  const run = spawnSync("git", args, { cwd, encoding: "utf8", windowsHide: true });
  if (run.error) throw run.error;
  assert.equal(run.status, 0, run.stderr);
}

async function invoke(browser, command, args = {}) {
  const result = await browser.execute(async (name, parameters) => {
    try {
      return { data: await window.__TAURI_INTERNALS__.invoke(name, parameters) };
    } catch (error) {
      return { error: String(error) };
    }
  }, command, args);
  if ("error" in result) throw new Error(result.error);
  return result.data;
}

async function startSession(application) {
  const browser = await remote({
    hostname: "127.0.0.1", port, path: "/", logLevel: "warn",
    connectionRetryCount: 0, connectionRetryTimeout: 15_000,
    capabilities: { browserName: "webview2", "tauri:options": { application } },
  });
  await browser.setTimeout({ script: 8_000 });
  return browser;
}

async function selectCheckout(browser, checkout) {
  // WebDriver operates inside WebView2. Use the same persisted selection command
  // as the file dialog after registering a repository.
  await invoke(browser, "select_active_checkout", { checkoutId: checkout.checkoutId });
  await browser.refresh();
  await browser.$("button=Start fixture").waitForDisplayed();
}

function readTabs(browser) {
  return browser.execute(() => ({
    groups: Array.from(document.querySelectorAll(".dv-groupview"), (group) =>
      Array.from(group.querySelectorAll(".dv-default-tab-content"), (tab) => tab.textContent)),
    active: document.querySelector(".dv-active-group .dv-active-tab .dv-default-tab-content")?.textContent ?? null,
  }));
}

test("registers a Git checkout and runs a fixture in the Windows desktop", { timeout: 90_000 }, async (t) => {
  assert.equal(process.platform, "win32");
  const root = await realpath(tmpdir());
  const scratch = await mkdtemp(join(root, "omniagent-webdriver-"));
  t.after(async () => {
    assert.equal(dirname(await realpath(scratch)), root);
    await rm(scratch, { recursive: true, force: true, maxRetries: 4, retryDelay: 500 });
  });
  const application = join(scratch, "omniagent.exe");
  await copyFile(resolve(process.env.OMNIAGENT_TEST_APP ?? "src-tauri/target/debug/omniagent.exe"), application);
  const repo = join(scratch, "Repositório com espaço");
  const linked = join(scratch, "Árvore linked");
  git(scratch, "init", "-q", repo);
  git(repo, "-c", "user.name=OmniAgent Fixture", "-c", "user.email=fixture@example.invalid",
    "-c", "commit.gpgsign=false", "commit", "-q", "--allow-empty", "-m", "Fixture");
  git(repo, "worktree", "add", "-q", "--detach", linked);

  const args = ["--port", String(port), "--native-port", String(port + 1)];
  if (process.env.OMNIAGENT_EDGE_DRIVER) args.push("--native-driver", process.env.OMNIAGENT_EDGE_DRIVER);
  const driver = spawn(process.env.OMNIAGENT_TAURI_DRIVER ?? "tauri-driver", args, {
    windowsHide: true, stdio: "ignore",
  });
  let browser;
  try {
    await Promise.race([
      waitForDriver(driver),
      new Promise((_, reject) => driver.once("error", reject)),
    ]);
    browser = await startSession(application);
    assert.equal(await invoke(browser, "plugin:app|identifier"), identifier);
    await browser.$("button=Open repository").waitForDisplayed();
    await assert.rejects(invoke(browser, "register_project", { directory: scratch }), /git repository/i);
    const main = await invoke(browser, "register_project", { directory: repo });
    const checkout = await invoke(browser, "register_project", { directory: linked });
    assert.equal(main.projectId, checkout.projectId);
    assert.notEqual(main.checkoutId, checkout.checkoutId);
    assert.equal(await realpath(checkout.checkoutRoot), await realpath(linked));
    await selectCheckout(browser, checkout);

    await browser.$("button=Start fixture").click();
    const input = await browser.$("form input");
    await input.waitForDisplayed();
    await input.setValue("Entrada de validação");
    await browser.$("button=Send input").click();
    const events = () => browser.$('ol[aria-label="Run events"]');
    await browser.waitUntil(async () => (await events().getText()).includes("Succeeded"));
    await browser.$("button=Inspect protected input").click();
    const evidence = browser.$("pre");
    await evidence.waitForDisplayed();
    assert.equal(await evidence.getText(), "Entrada de validação");

    await browser.$('button[aria-label="Move tab left"]').click();
    const firstHeading = await browser.$(".dv-content-container h1").getText();
    assert.deepEqual((await readTabs(browser)).groups, [[firstHeading, "Fixture runs"]]);
    await browser.$('aside[aria-label="Workspace"] button').click();
    await browser.$("button=Start fixture").click();
    await browser.$("button=Cancel run").waitForDisplayed();
    const secondHeading = await browser.$(".dv-content-container h1").getText();
    assert.notEqual(firstHeading, secondHeading);
    await browser.$("button=Cancel run").click();
    await browser.waitUntil(async () => (await events().getText()).includes("Cancelled"));
    await browser.$("button=Split right").click();
    const expectedTabs = { groups: [[firstHeading, "Fixture runs"], [secondHeading]], active: secondHeading };
    await browser.waitUntil(async () => (await readTabs(browser)).groups.length === 2);
    assert.deepEqual(await readTabs(browser), expectedTabs);
    await browser.waitUntil(async () => {
      const saved = await invoke(browser, "workbench_layout", { checkoutId: checkout.checkoutId, workspace: "fixtures" });
      return saved && JSON.parse(saved).grid.root.data.length === 2;
    });

    await browser.deleteSession();
    browser = null;
    browser = await startSession(application);
    await browser.waitUntil(async () => (await readTabs(browser)).groups.length === 2);
    assert.deepEqual(await readTabs(browser), expectedTabs);
    assert.equal(await browser.$("header span[title]").getAttribute("title"), checkout.checkoutRoot);
  } finally {
    try {
      await browser?.deleteSession();
    } finally {
      driver.kill();
    }
  }
});
