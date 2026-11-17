import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { createHash, randomUUID } from "node:crypto";
import { access, copyFile, mkdir, mkdtemp, readFile, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, dirname, join, resolve } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { Key, remote } from "webdriverio";

import { verifyAuditFactory, verifyVisualPipelineBuilder } from "./visual-pipeline-builder.mjs";

const fixtureIdentifier = "io.github.legustavinho.omniagent.desktop-tests";
const packagedOrigin = "http://tauri.localhost";
const port = Number(process.env.OMNIAGENT_WEBDRIVER_PORT ?? 4444);

function fixtureRunId(application) {
  const match = /^omniagent-webdriver-([0-9a-f]{32})-/.exec(basename(dirname(application)));
  assert.ok(match, "The copied desktop fixture needs an isolated run ID");
  return match[1];
}

function testIdentifier(application) {
  return `${fixtureIdentifier}.run-${fixtureRunId(application)}`;
}

function fixtureEnvironment(application) {
  const { WEBVIEW2_USER_DATA_FOLDER: _, ...env } = process.env;
  return { ...env, OMNIAGENT_DESKTOP_TEST_RUN_ID: fixtureRunId(application) };
}

let localAppData;
function fixtureProfile(application) {
  if (!localAppData) {
    const result = spawnSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command",
      "[Environment]::GetFolderPath([Environment+SpecialFolder]::LocalApplicationData)"],
    { windowsHide: true, encoding: "utf8" });
    if (result.error) throw result.error;
    assert.equal(result.status, 0, result.stderr);
    localAppData = result.stdout.trim();
    assert.ok(localAppData, "Windows did not report the local application data directory");
  }
  return join(localAppData, testIdentifier(application));
}

async function removeFixtureProfile(application) {
  const profile = fixtureProfile(application);
  const actual = await realpath(profile).catch((error) => {
    if (error.code === "ENOENT") return null;
    throw error;
  });
  if (!actual) return;
  assert.equal(dirname(actual), await realpath(localAppData));
  assert.equal(basename(actual), testIdentifier(application));
  await rm(actual, { recursive: true, force: true, maxRetries: 4, retryDelay: 500 });
}

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
  return run.stdout;
}

async function invoke(browser, command, args = {}) {
  const result = await browser.execute(async (name, parameters) => {
    try {
      return { data: await window.__TAURI_INTERNALS__.invoke(name, parameters) };
    } catch (error) {
      return { ipcError: String(error) };
    }
  }, command, args);
  if ("ipcError" in result) throw new Error(result.ipcError);
  return result.data;
}

async function startSession(application) {
  const browser = await remote({
    hostname: "127.0.0.1", port, path: "/", logLevel: "warn",
    connectionRetryCount: 0, connectionRetryTimeout: 15_000,
    capabilities: { browserName: "webview2", "wdio:enforceWebDriverClassic": true, "tauri:options": { application } },
  });
  await browser.setTimeout({ script: 8_000 });
  try {
    await waitForRuntime(browser, { startup: true });
    assert.equal(await invoke(browser, "plugin:app|identifier"), testIdentifier(application));
  } catch (error) {
    const profile = fixtureProfile(application);
    const files = await Promise.all(["installation.dpapi", "omniagent.sqlite3"].map(async (name) =>
      [name, await access(join(profile, name)).then(() => true, () => false)]));
    await browser.deleteSession().catch(() => {});
    throw new Error(`Desktop fixture did not start (${JSON.stringify(Object.fromEntries(files))})`, { cause: error });
  }
  return browser;
}

async function waitForRuntime(browser, { startup = false } = {}) {
  const state = { browserVersion: browser.capabilities.browserVersion };
  try {
    await browser.waitUntil(async () => {
      try {
        state.document = await browser.execute(() => ({
          url: location.href, origin: location.origin, title: document.title,
          readyState: document.readyState, hasInvoke: typeof window.__TAURI_INTERNALS__?.invoke === "function",
        }));
        if (state.document.origin !== packagedOrigin || !state.document.hasInvoke) return false;
        state.runtime = await browser.execute(async () => {
          try {
            return await Promise.race([
              window.__TAURI_INTERNALS__.invoke("runtime_status"),
              new Promise((resolve) => setTimeout(() => resolve("Runtime command timed out"), 2_000)),
            ]);
          } catch (error) {
            return String(error);
          }
        });
        return state.runtime === "Desktop runtime is ready.";
      } catch (error) {
        state.webdriverError = String(error);
        return false;
      }
    }, { timeout: startup ? 30_000 : 15_000 });
  } catch (error) {
    throw new Error(`Desktop runtime did not become ready (${JSON.stringify(state)})`, { cause: error });
  }
}

async function waitForFixtureResult(browser, projectId, conversationId, label, timeout) {
  const scope = { conversationId };
  let run;
  let pollFailure;
  try {
    await browser.waitUntil(async () => {
      run = await invoke(browser, "latest_conversation_run", scope);
      return ["succeeded", "failed", "cancelled", "interrupted"].includes(run?.status);
    }, { timeout });
  } catch (error) {
    pollFailure = error;
  }
  if (run?.status === "succeeded" && !pollFailure) return run;
  const job = await invoke(browser, "latest_conversation_job", scope).catch((error) => ({ error: String(error) }));
  const failure = run && await invoke(browser, "job_failure", { projectId, runId: run.id }).catch(() => undefined);
  const state = { runId: run?.id, status: run?.status, jobId: job?.job?.id, queueState: job?.job?.queueState,
    blockers: job?.blockers, inspectionError: job?.error, failure };
  throw new Error(`${label} did not succeed (${JSON.stringify(state)})`, { cause: pollFailure });
}

async function showTestApplication(browser, application, args = []) {
  const reopen = spawn(application, args, { stdio: "ignore", windowsHide: true, env: fixtureEnvironment(application) });
  const stopped = new Promise((resolve, reject) => {
    reopen.once("error", reject);
    reopen.once("exit", resolve);
  });
  try {
    await browser.waitUntil(() => invoke(browser, "plugin:window|is_visible", { label: "main" }));
    assert.equal(await stopped, 0);
  } finally {
    if (reopen.exitCode === null) reopen.kill();
  }
}

async function selectCheckout(browser, checkout) {
  // WebDriver operates inside WebView2. Use the same persisted selection command
  // as the file dialog after registering a repository.
  await invoke(browser, "select_active_checkout", { checkoutId: checkout.checkoutId });
  await browser.refresh();
  await browser.$('button[aria-label="Diagnostics"]').waitForDisplayed();
}

function readTabs(browser) {
  return browser.execute(() => {
    const view = document.querySelector(".work-deck:not([hidden]) .work-view:not([hidden])");
    return {
      groups: Array.from(view?.querySelectorAll(".dv-groupview") ?? [], (group) =>
        Array.from(group.querySelectorAll(".dv-default-tab-content"), (tab) => tab.textContent)),
      active: view?.querySelector(".dv-active-group .dv-active-tab .dv-default-tab-content")?.textContent ?? null,
    };
  });
}

async function chooseConversations(browser) {
  await browser.$('button[aria-label="Conversations"]').click();
  const create = browser.$('button[aria-label="New conversation"]');
  if (!(await create.isDisplayed())) {
    const sidebar = browser.$('button[aria-label="Toggle conversation navigation"]');
    if ((await sidebar.getAttribute("aria-expanded")) === "false") await sidebar.click();
  }
  await create.waitForDisplayed();
}

async function click(browser, selector, wait) {
  const control = browser.$(selector);
  await control.waitForDisplayed(wait);
  await control.waitForEnabled(wait);
  await control.click();
}

async function replaceControlValue(browser, control, value) {
  await control.click();
  await browser.action("key").down(Key.Control).down("a").up("a").up(Key.Control).perform();
  await control.addValue(value);
  assert.equal(await control.getValue(), value);
}

async function clickConversation(browser, title) {
  await click(browser, `section[aria-label="Conversation library"] button[aria-label=${JSON.stringify(title)}]`);
}

async function capture(browser, name) {
  const directory = process.env.OMNIAGENT_UI_CAPTURE_DIR;
  if (!directory) return;
  await mkdir(directory, { recursive: true });
  await browser.execute(async () => {
    await Promise.allSettled(document.getAnimations().filter((animation) => animation.animationName === "ui-reveal")
      .map((animation) => animation.finished));
  });
  const { data } = await emulateDisplay(browser, "Page.captureScreenshot", { captureBeyondViewport: false, fromSurface: true });
  await writeFile(join(directory, name + ".png"), Buffer.from(data, "base64"));
}

async function emulateDisplay(browser, cmd, params = {}) {
  const endpoint = "http://127.0.0.1:" + (port + 1) + "/session/" + encodeURIComponent(browser.sessionId) + "/ms/cdp/execute";
  const response = await fetch(endpoint, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ cmd, params }),
    signal: AbortSignal.timeout(5000),
  });
  const result = await response.text();
  assert.ok(response.ok, cmd + ": " + result);
  return JSON.parse(result).value;
}

async function checkDisplayPreferences(browser) {
  await invoke(browser, "plugin:window|set_size", { label: "main", value: { Logical: { width: 1120, height: 760 } } });
  for (const factor of [1.25, 1.5]) {
    await emulateDisplay(browser, "Emulation.setDeviceMetricsOverride", {
      width: Math.floor(1120 / factor), height: Math.floor(760 / factor), deviceScaleFactor: factor, mobile: false,
    });
    assert.equal(await browser.execute(() => window.devicePixelRatio), factor);
    assert.equal(await browser.execute(() => document.documentElement.scrollWidth <= window.innerWidth + 1), true);
    await capture(browser, "06-scale-" + factor * 100);
  }
  await emulateDisplay(browser, "Emulation.clearDeviceMetricsOverride");
  await emulateDisplay(browser, "Emulation.setEmulatedMedia", { features: [{ name: "forced-colors", value: "active" }] });
  assert.equal(await browser.execute(() => matchMedia("(forced-colors: active)").matches), true);
  assert.equal(await browser.execute(() =>
    getComputedStyle(document.querySelector('.work-rail-button[aria-current="page"]')).borderTopWidth), "1px");
  await capture(browser, "07-high-contrast");
  await emulateDisplay(browser, "Emulation.setEmulatedMedia", { features: [{ name: "prefers-reduced-motion", value: "reduce" }] });
  await browser.waitUntil(() => browser.execute(() => document.documentElement.dataset.appMotion === "reduced"));
  await capture(browser, "07-reduced-motion");
  await emulateDisplay(browser, "Emulation.setEmulatedMedia", { features: [] });

  const originalTheme = (await invoke(browser, "appearance_settings")).theme;
  async function useTheme(theme) {
    await invoke(browser, "plugin:window|set_size", { label: "main", value: { Logical: { width: 1120, height: 760 } } });
    await click(browser, '.work-rail button[aria-label="Settings"]');
    const settings = browser.$('.settings-dialog');
    await click(settings, './/nav[@aria-label="Settings categories"]//button[span[normalize-space(.)="Appearance"]]');
    const themeControl = settings.$('[data-setting="theme"]');
    await themeControl.waitForDisplayed();
    await click(themeControl, "button=" + theme[0].toUpperCase() + theme.slice(1));
    await browser.waitUntil(async () => (await invoke(browser, "appearance_settings")).theme === theme);
    await click(settings, 'button[aria-label="Close settings"]');
  }
  const areas = ["Conversations", "IDE", "Tasks", "Pipelines", "Git", "Diagnostics", "Knowledge"];
  for (const theme of ["light", "dark"]) {
    await useTheme(theme);
    for (const [width, height] of [[1440, 900], [1120, 760], [640, 440]]) {
      await invoke(browser, "plugin:window|set_size", { label: "main", value: { Logical: { width, height } } });
      await browser.waitUntil(() => browser.execute((size) => Math.abs(innerWidth - size) <= 4, width));
      for (const area of areas) {
        await click(browser, `.work-rail button[aria-label="${area}"]`);
        await browser.waitUntil(() => browser.execute((title) => {
          const current = document.querySelector('.work-rail-button[aria-current="page"]');
          return current?.getAttribute("aria-label") === title &&
            !document.querySelector('.work-deck:not([hidden]) .work-view:not([hidden]) > .work-progress');
        }, area));
        assert.equal(await browser.execute(() => document.documentElement.scrollWidth <= innerWidth + 1), true, `${area} in ${theme} at ${width}`);
        await capture(browser, `workspace-${area.toLowerCase()}-${theme}-${width}x${height}`);
      }
    }
  }
  await useTheme(originalTheme);
}

async function checkSharedWorkspaceNavigation(browser, expectedCapacity) {
  const views = [
    ["Conversations", ".conv-workspace"],
    ["IDE", '[aria-label="IDE workspace"] button[aria-label="Toggle bottom panel"]'],
    ["Tasks", '[aria-label="Tasks workspace"] [aria-label="Agile view"]'],
    ["Pipelines", '[aria-label="Pipelines workspace"] [aria-label="Pipelines views"]'],
    ["Git", '[aria-label="Git workspace"] [aria-label="Git views"]'],
    ["Diagnostics", '[aria-label="Diagnostics workspace"] [aria-label="Diagnostic views"]'],
    ["Knowledge", '[aria-label="Knowledge workspace"] button[aria-label="Note details"]'],
  ];
  const shown = ".work-deck:not([hidden]) .work-view:not([hidden])";
  for (const [area, landmark] of views) {
    const rail = `.work-rail button[aria-label="${area}"]`;
    await click(browser, rail);
    assert.equal(await browser.$(rail).getAttribute("aria-current"), "page");
    const pane = browser.$(shown);
    await pane.$(landmark).waitForDisplayed({ timeoutMsg: `${area} did not render its shared controls` });
    if (area === "Tasks") {
      await click(pane, "button=Table");
      await pane.$('table[aria-label="Agile table"]').waitForDisplayed();
      await click(pane, "button=Kanban");
    }
    assert.equal(await browser.execute(() => document.documentElement.scrollWidth <= window.innerWidth + 1), true, area);
  }
  if (await browser.execute(() => innerWidth < 1040)) {
    const goTo = 'button[aria-label="Go to…"]';
    await click(browser, goTo);
    const search = browser.$('[role="combobox"][aria-label="Search destinations"]');
    await search.waitForDisplayed();
    await search.setValue("Git");
    await browser.keys("Enter");
    await browser.$('.work-rail button[aria-label="Git"][aria-current="page"]').waitForDisplayed();
    await click(browser, '.work-rail button[aria-label="Knowledge"]');
  }
  const queueButton = '.work-rail button[aria-label="Queue"]';
  const wasOpen = await browser.$(queueButton).getAttribute("aria-pressed") === "true";
  if (!wasOpen) await click(browser, queueButton);
  await browser.$('.work-queue-panel:not([hidden]) [aria-label="Queue states"]').waitForDisplayed();
  if (!wasOpen) {
    const drawer = browser.$('.work-queue-drawer[role="dialog"]');
    if (await drawer.isDisplayed()) {
      await browser.keys("Escape");
      await drawer.waitForDisplayed({ reverse: true });
      await browser.waitUntil(() => browser.execute(() => document.activeElement?.getAttribute("aria-label") === "Queue"));
    } else await click(browser, '.work-queue-panel:not([hidden]) button[aria-label="Close queue"]');
  }
  await click(browser, '.work-rail button[aria-label="Settings"]');
  const settings = browser.$('.settings-dialog');
  await settings.$('[aria-label="Settings categories"]').waitForDisplayed();
  await click(settings, 'button=Execution capacity');
  const capacity = settings.$('input#managed-capacity');
  await capacity.waitForEnabled();
  assert.equal(await capacity.getValue(), String(expectedCapacity));
  await click(browser, 'button[aria-label="Close settings"]');
}

async function checkConversationActivity(browser) {
  const inspectorSelector = '.work-inspector[data-kind="activity"]';
  const tabsSelector = '[role="tablist"][aria-label="Run details"]';
  const activity = browser.$(inspectorSelector);
  const tabs = activity.$(tabsSelector);
  await tabs.waitForDisplayed();
  const names = await browser.execute((selector) =>
    Array.from(document.querySelectorAll(selector), (tab) => tab.textContent), `${inspectorSelector} ${tabsSelector} [role="tab"]`);
  assert.deepEqual(names, ["Overview", "Timeline", "Agents", "Context", "Exact input", "Approvals", "Artifacts"]);
  await click(tabs, 'button=Timeline');
  await activity.$('h3=Events').waitForDisplayed();
  return activity;
}

async function checkSimulation(browser, checkout) {
  const conversationScope = { checkoutId: checkout.checkoutId };
  const before = await invoke(browser, "list_conversations", conversationScope);
  const settingsBefore = await invoke(browser, "appearance_settings");
  const queueBefore = await invoke(browser, "project_job_queue", { projectId: checkout.projectId });
  const realCapacity = queueBefore.settings.capacity;
  await invoke(browser, "plugin:window|set_size", { label: "main", value: { Logical: { width: 1440, height: 900 } } });
  await checkSharedWorkspaceNavigation(browser, realCapacity);
  await click(browser, '.work-titlebar button[title="Try a workflow with example data"]');
  const simulation = browser.$('[aria-label="Simulation controls"]');
  await simulation.waitForDisplayed();
  await checkSharedWorkspaceNavigation(browser, 3);

  const shown = ".work-deck:not([hidden]) .work-view:not([hidden])";
  const knowledge = browser.$(shown);
  await click(knowledge, "button=Reading");
  await knowledge.$(".note-reading article").waitForDisplayed();
  await capture(browser, "08-knowledge-reading");
  await click(knowledge, "button=Graph");
  await knowledge.$(".note-graph-canvas canvas:first-child").waitForDisplayed();
  await capture(browser, "08-knowledge-graph");
  await click(knowledge, "button=Graph");
  await click(browser, "button=Plan from note");
  await browser.$("strong=Who should the recovery instructions address?").waitForDisplayed();
  await click(browser, 'button[aria-label="Tasks"]');
  const taskWorkspace = browser.$(shown);
  await click(taskWorkspace, "button=Add release readiness checklist");
  await click(taskWorkspace, "button=Answer question");
  await browser.$("strong=Who should the recovery instructions address?").waitForDisplayed();
  await click(browser, 'button[aria-label="Back to Tasks"]');
  await taskWorkspace.$('[aria-label="Task details"]').$("button=Answer question").waitForDisplayed();
  await browser.waitUntil(() => browser.execute(() => document.activeElement?.textContent?.trim() === "Answer question"));
  await click(taskWorkspace, "button=Answer question");
  await capture(browser, "01-conversations");
  const conversationRow = browser.$(".conv-example-sidebar").$("button=Plan the release checklist");
  await conversationRow.click({ button: "right" });
  await browser.$('[role="menu"]').waitForDisplayed();
  await capture(browser, "01-context-menu");
  await browser.keys("Escape");
  const conversation = browser.$(shown);
  const composerHeight = () => browser.execute(() => document.querySelector(".work-deck:not([hidden]) .work-view:not([hidden]) .conv-composer")
    .getBoundingClientRect().height);
  const heightBefore = await composerHeight();
  await click(conversation, ".model-picker-trigger");
  const modelMenu = browser.$(".model-picker-menu");
  await modelMenu.waitForDisplayed();
  assert.equal(await composerHeight(), heightBefore);
  assert.equal(await browser.execute(() => {
    const header = document.querySelector(".model-picker-catalog-header");
    const heading = header.querySelector(".model-picker-heading").getBoundingClientRect();
    const sort = header.querySelector(".model-picker-sort").getBoundingClientRect();
    return Math.abs(heading.top + heading.height / 2 - sort.top - sort.height / 2) <= 1;
  }), true, "Model sorting and title must share a line when the menu has room");
  assert.equal(await conversation.$('select[aria-label="Provider"]').isExisting(), false);
  await modelMenu.$('[role="menuitemradio"][aria-label="Gemini 4 Hydrogen"]').waitForDisplayed();
  await click(modelMenu, '[aria-label="Price, lowest first"]');
  await modelMenu.$('[aria-label="Price, lowest first"][aria-checked="true"]').waitForDisplayed();
  await capture(browser, "01-composer-model-picker");
  await click(browser, '.model-picker-menu [role="menuitemradio"][aria-label="GPT‑6.1 Sol"]');
  const effortValues = () => browser.execute(() => {
    const composer = document.querySelector(".work-deck:not([hidden]) .work-view:not([hidden]) .conv-composer");
    return Array.from(composer.querySelector('select[aria-label="Effort"]').options, (option) => option.value);
  });
  assert.deepEqual(await effortValues(), ["", "low", "medium", "high", "xhigh", "max", "ultra"]);
  const effort = conversation.$('select[aria-label="Effort"]');
  await effort.selectByAttribute("value", "ultra");
  await click(conversation, ".model-picker-trigger");
  await click(browser, '.model-picker-menu [role="menuitemradio"][aria-label="GPT‑6 Luna"]');
  assert.deepEqual(await effortValues(), ["", "low", "medium", "high", "xhigh", "max"]);
  assert.equal(await effort.getValue(), "");
  await click(conversation, ".model-picker-trigger");
  await click(browser, '.model-picker-menu [role="menuitemradio"][aria-label="Gemini 4 Hydrogen"]');
  assert.deepEqual(await effortValues(), ["", "low", "medium", "high", "xhigh"]);
  await click(conversation, ".model-picker-trigger");
  await click(browser, '.model-picker-menu [role="menuitemradio"][aria-label="GPT‑6.1 Sol"]');
  await conversation.$('select[aria-label="Conversation mode"]').selectByAttribute("value", "goal");
  const draft = conversation.$('textarea[aria-label="Message draft"]');
  await draft.setValue("Verify recovery instructions against the release checklist.");
  await click(conversation, "button=Start goal");
  await click(conversation, "button=Continue goal");
  await click(conversation, 'button[aria-label="View goal objective"]');
  const goalDialog = browser.$('.conv-dialog[role="dialog"]');
  const objective = goalDialog.$("textarea");
  assert.equal(await objective.getValue(), "Verify recovery instructions against the release checklist.");
  await replaceControlValue(browser, objective, "Verify backup and recovery instructions against the release checklist.");
  await capture(browser, "01-goal-editor");
  await click(goalDialog, "button=Save objective");
  assert.match(await conversation.$('.conv-goal [role="status"]').getText(), /Running · 1\/3 turns/);
  await click(conversation, "button=Cancel goal");
  await conversation.$('select[aria-label="Conversation mode"]').selectByAttribute("value", "plan");
  await click(conversation, "button=Reply");
  await click(conversation, "button=Approve implementation");
  await click(conversation, "button=Open release task");
  const taskModel = 'button[aria-label^="Filter tasks by model ·"]';
  await click(browser.$(shown), taskModel);
  await click(browser, '.model-picker-menu [aria-label="GPT-6.1 Sol"]');
  assert.equal(await browser.$(shown).$(taskModel).getText(), "GPT-6.1 Sol");
  await click(browser.$(shown), taskModel);
  await click(browser.$(".model-picker-menu"), 'div=All models');
  assert.equal(await browser.$(shown).$(taskModel).getText(), "All models");
  await capture(browser, "09-agile-kanban");
  await click(browser.$(shown), "button=Table");
  const agile = browser.$(shown).$(".agile-table");
  await click(agile, 'button[aria-label="Expand Windows release readiness"]');
  await click(agile, 'button[aria-label="Expand Add release readiness checklist"]');
  const checklistItem = agile.$('label=Review the candidate diff').$('input[type="checkbox"]');
  assert.equal(await checklistItem.isSelected(), false);
  await checklistItem.click();
  assert.equal(await checklistItem.isSelected(), true);
  await click(agile, 'button[aria-label="Expand Document the rollback exercise"]');
  await agile.$('label=List the backup and restore requirements').waitForDisplayed();
  await capture(browser, "09-agile-table");
  await click(browser.$(shown), "button=Kanban");
  await click(browser, 'button[aria-label="Queue"]');
  const queue = browser.$('.work-queue-panel:not([hidden]) [aria-label="Execution queue"]');
  await queue.waitForDisplayed();
  await capture(browser, "10-queue");
  await click(browser, "button=Start attempt");
  await click(browser, "button=Capture candidate");
  await click(queue, 'button=Inspect attempt');
  const activity = await checkConversationActivity(browser);
  await capture(browser, "10-activity-simulate");
  await click(activity, 'button[aria-label="Close activity"]');
  await click(queue, 'button[aria-label="Close queue"]');
  await click(browser.$(shown), "button=Run verification");
  await click(browser.$(shown), "button=Review saved diff");
  const changes = () => browser.$('.work-inspector[data-kind="changes"]');
  await changes().$('.monaco-diff-editor').waitForDisplayed({ timeout: 20_000 });
  await click(changes(), 'button[aria-label="Expand side panel"]');
  await changes().$('select[aria-label="Diff layout"]').selectByAttribute("value", "split");
  await capture(browser, "02-saved-changes");
  await changes().$('select[aria-label="Diff layout"]').selectByAttribute("value", "unified");
  await capture(browser, "02-unified-changes");
  await click(changes(), 'button[aria-label="Close changes"]');
  await browser.waitUntil(() => browser.execute(() => document.activeElement?.textContent?.trim() === "Review saved diff"),
    { timeoutMsg: "Closing Changes did not restore focus to the task's diff action" });
  await click(browser.$(shown), "button=Review saved diff");
  await click(changes(), "button=Accept diff");
  await click(changes(), 'button[aria-label="Close changes"]');
  await browser.$(shown).$('[aria-label="Task details"] [role="alert"]').waitForDisplayed();
  assert.equal(await browser.$(shown).$('[aria-label="Task details"] [role="alert"]').getText(), "Waiting for Sub-Tasks to finish.");

  await click(browser, 'button[aria-label="IDE"]');
  const ide = browser.$(shown);
  if (!await ide.$('button[aria-expanded="true"][title="src"]').isExisting()) await click(ide, 'button[title="src"][aria-expanded]');
  await click(ide, "button=index.ts");
  await ide.$(".monaco-editor").waitForDisplayed();
  const bottom = ide.$('button[aria-label="Toggle bottom panel"]');
  if ((await bottom.getAttribute("aria-expanded")) === "false") await bottom.click();
  await ide.$(".xterm").waitForDisplayed();
  await capture(browser, "11-editor-and-terminal");

  const scenario = simulation.$("select");
  await scenario.selectByAttribute("value", "recovery");
  await click(browser, 'button[aria-label="Queue"]');
  await click(browser, "button=Resume from checkpoint");
  await browser.$("button=Start attempt").waitForEnabled();
  await scenario.selectByAttribute("value", "pipelines");
  await browser.$(shown).$(".react-flow").waitForDisplayed();
  await capture(browser, "03-pipelines");
  await click(browser.$(shown), "button=Roles & templates");
  const roles = browser.$(shown).$(".pipeline-view:not([hidden])");
  const roleModel = 'button[aria-label^="Default model ·"]';
  await click(roles, roleModel);
  await browser.$('.model-picker-menu [aria-label="GPT‑6.1 Sol"][aria-checked="true"]').waitForDisplayed();
  await capture(browser, "03-role-model-picker");
  await click(browser, '.model-picker-menu [aria-label="GPT‑6 Luna"]');
  assert.equal(await roles.$(roleModel).getText(), "GPT‑6 Luna");
  await click(roles, roleModel);
  await click(browser, '.model-picker-menu [aria-label="GPT‑6.1 Sol"]');
  await click(browser.$(shown), "button=Compare");
  await click(browser.$(shown).$(".pipeline-view:not([hidden])"), 'button[aria-label="Select models · 3 selected"]');
  await click(browser, '.model-picker-menu [role="menuitemcheckbox"][aria-label="GPT‑6 Luna"]');
  assert.equal(await browser.$('.model-picker-menu [role="menuitemcheckbox"][aria-label="GPT‑6 Luna"]').getAttribute("aria-checked"), "false");
  await click(browser, '.model-picker-menu [role="menuitemcheckbox"][aria-label="GPT‑6 Luna"]');
  await browser.keys("Escape");
  await click(browser.$(shown), "button=Library");
  const catalog = browser.$(shown).$(".pipeline-view:not([hidden])");
  await catalog.$('input[aria-label="Search pipeline nodes"]').setValue("verified source commit");
  await catalog.$("h4=Commit").waitForDisplayed();
  await catalog.$("h4=Merge").waitForDisplayed();
  await capture(browser, "03-node-library");
  await click(browser.$(shown).$('[aria-label="Pipelines views"]'), "button=Builder");
  const demoCanvas = browser.$(shown).$('[aria-label="Simulation graph canvas"]');
  await demoCanvas.$(".workflow-node-card[data-selected]").waitForDisplayed();
  await demoCanvas.$(".react-flow__controls-fitview").waitForDisplayed();
  await click(browser.$(shown), "button=Add node");
  const nodePicker = browser.$('.pipeline-catalog-dialog');
  await nodePicker.$('input[aria-label="Search pipeline nodes"]').setValue("result artifact");
  await nodePicker.$("h4=Result").waitForDisplayed();
  assert.equal(await nodePicker.$("button=Add").isExisting(), false);
  await click(nodePicker, 'button[aria-label="Close node catalog"]');
  await click(browser, 'button[aria-label="Settings"]');
  await click(browser, "button=Git and delivery");
  const gitMode = browser.$('select[id="after-acceptance"]');
  await gitMode.waitForEnabled();
  assert.equal(await gitMode.getValue(), "automatic_commit_user_merge");
  await gitMode.selectByAttribute("value", "user_commits_and_merges");
  await click(browser, 'button[aria-label="Close settings"]');
  const pipeline = browser.$(shown);
  const stageWait = { timeout: 15_000 };
  await click(pipeline, "button=Run pipeline");
  await click(pipeline, "button=View waiting stage", stageWait);
  await click(pipeline, "button=Answer question");
  await click(pipeline, "button=View waiting stage", stageWait);
  await click(pipeline, "button=Approve implementation");
  await click(pipeline, "button=View failed stage", stageWait);
  await click(pipeline, "button=Retry stage");
  await click(pipeline, "button=View waiting stage", stageWait);
  await click(pipeline, "button=Accept diff");
  await click(pipeline, "button=View waiting stage", stageWait);
  const gitWait = pipeline.$(".pipeline-inspector-scroll > .pipeline-stack > p");
  await gitWait.waitForDisplayed();
  assert.equal(await gitWait.getText(), "Waiting for commit");
  await capture(browser, "03-pipeline-waiting-commit");
  await click(pipeline, "button=Open Git delivery");
  await click(browser.$(shown), "button=Commit accepted candidate");
  await capture(browser, "12-git-delivery");
  await click(browser, 'button[aria-label="Pipelines"]');
  await browser.$(shown).$(".pipeline-toolbar .pipeline-status[data-status='succeeded']").waitForDisplayed();
  await scenario.selectByAttribute("value", "windows");
  await click(browser, "button=Create backup");
  await click(browser, "button=Continue");
  await click(browser, "button=Continue");
  await click(browser, "button=Finish");
  await capture(browser, "04-system-settings");
  await click(browser, "button=Execution policies");
  await click(browser.$('.settings-dialog'), 'button[aria-label^="Planning model ·"]');
  await click(browser, '.model-picker-menu [aria-label="GPT‑6.1 Sol"]');
  assert.equal(await browser.$('.settings-dialog button[aria-label^="Planning model ·"]').getText(), "GPT‑6.1 Sol");
  await click(browser, "button=Appearance");
  await capture(browser, "04-appearance-settings");
  await click(browser, 'button[aria-label="Close settings"]');
  await scenario.selectByAttribute("value", "pipelines");
  await invoke(browser, "plugin:window|set_size", { label: "main", value: { Logical: { width: 640, height: 440 } } });
  await browser.waitUntil(() => browser.execute(() => Math.abs(window.innerWidth - 640) <= 4));
  await checkSharedWorkspaceNavigation(browser, 3);
  await click(browser, 'button[aria-label="Settings"]');
  await click(browser, "button=Appearance");
  await browser.$('[data-setting="theme"]').waitForDisplayed();
  assert.equal(await browser.execute(() => {
    const dialog = document.querySelector(".settings-dialog").getBoundingClientRect();
    return dialog.top >= 0 && dialog.left >= 0 && dialog.right <= innerWidth && dialog.bottom <= innerHeight;
  }), true, "Compact Settings must stay inside the window while it opens");
  await capture(browser, "04-settings-compact");
  await browser.keys("Escape");
  await browser.waitUntil(() => browser.execute(() => document.activeElement?.getAttribute("aria-label") === "Settings"));
  await click(browser, "button=Exit simulation");
  await simulation.waitForExist({ reverse: true });
  assert.equal(await browser.$('button[aria-label="Knowledge"]').getAttribute("aria-current"), "page");
  assert.deepEqual(await browser.execute(() => {
    const shown = [...document.querySelectorAll(".work-view")].filter((view) => view.getClientRects().length);
    return shown.map((view) => view.querySelector("[aria-label]")?.getAttribute("aria-label"));
  }), ["Knowledge workspace"]);
  assert.equal((await invoke(browser, "active_checkout")).checkoutId, checkout.checkoutId);
  assert.deepEqual(await invoke(browser, "list_conversations", conversationScope), before);
  assert.deepEqual(await invoke(browser, "appearance_settings"), settingsBefore);
  for (const [width, height] of [[1440, 900], [1120, 760], [640, 440]]) {
    await invoke(browser, "plugin:window|set_size", { label: "main", value: { Logical: { width, height } } });
    await browser.waitUntil(() => browser.execute((wanted) => Math.abs(window.innerWidth - wanted) <= 4, width));
    assert.equal(await browser.execute(() => document.documentElement.scrollWidth <= window.innerWidth + 1), true);
    if (width === 640) await checkSharedWorkspaceNavigation(browser, realCapacity);
    await capture(browser, "05-workspace-" + width);
  }
  if (process.env.OMNIAGENT_UI_TEST_DISPLAY === "1") await checkDisplayPreferences(browser);
}

function killTestApplication(application) {
  const stopped = spawnSync("powershell.exe", [
    "-NoProfile", "-NonInteractive", "-Command",
    `$app = Get-CimInstance Win32_Process -Filter "Name = 'omniagent.exe'" |
      Where-Object { $_.ExecutablePath -eq $env:OMNIAGENT_TEST_RESTART_EXE };
      if (@($app).Count -ne 1) { throw 'Expected exactly one desktop test process' };
      Stop-Process -Id $app.ProcessId -Force -ErrorAction Stop`,
  ], {
    windowsHide: true,
    encoding: "utf8",
    env: { ...process.env, OMNIAGENT_TEST_RESTART_EXE: application },
  });
  if (stopped.error) throw stopped.error;
  assert.equal(stopped.status, 0, stopped.stderr);
}

function runRows(browser, runId) {
  return browser.execute((prefix) => {
    const timeline = Array.from(document.querySelectorAll('ol[aria-label="Run events"]')).find(
      (list) => list.closest("section")?.querySelector("h1")?.textContent === "Run " + prefix,
    );
    return timeline ? Array.from(timeline.children, (row) => row.querySelector("span + span")?.textContent) : [];
  }, runId.slice(0, 8));
}

async function verifyDetachedJob(browser, application, scratch) {
  const repo = join(scratch, "Detached queue checkout");
  git(scratch, "init", "-q", repo);
  git(repo, "-c", "user.name=OmniAgent Fixture", "-c", "user.email=fixture@example.invalid",
    "-c", "commit.gpgsign=false", "commit", "-q", "--allow-empty", "-m", "Fixture");
  const checkout = await invoke(browser, "register_project", { directory: repo });
  await selectCheckout(browser, checkout);
  const page = await browser.getUrl();
  const release = join(repo, "release-desktop-jobs");
  const profile = (await invoke(browser, "cli_executables")).find(({ provider }) => provider === "codex");
  assert.ok(profile);
  await invoke(browser, "set_cli_executable", { provider: "codex", executable: application });

  let detached = false;
  let pausedByTest = false;
  try {
    const models = await invoke(browser, "conversation_models", { provider: "codex" });
    const fixture = models.find(({ option }) => option.selection.modelId === "desktop-fixture" && option.selectable);
    assert.ok(fixture, "the desktop fixture must supply the configured provider");
    const queue = () => invoke(browser, "project_job_queue", { projectId: checkout.projectId });
    const initial = await queue();
    assert.equal(initial.settings.paused, false);
    assert.equal(initial.active, 0);
    const capacity = initial.settings.capacity;
    assert.ok(capacity > 0);

    async function enqueue(title, text) {
      const chat = await invoke(browser, "create_conversation", { checkoutId: checkout.checkoutId, title });
      await invoke(browser, "save_conversation_draft", { conversationId: chat.id, text });
      const job = await invoke(browser, "start_conversation_turn", {
        conversationId: chat.id, selection: fixture.option.selection, mode: "read_only", text,
      });
      return { chat, job };
    }

    const holders = [];
    for (let slot = 1; slot <= capacity; slot++) {
      holders.push((await enqueue(`Holder ${slot}`, `Hold desktop slot ${slot}`)).job);
    }
    const markers = holders.map((_, index) => join(repo, `desktop-slot-${index + 1}`));
    await browser.waitUntil(async () => {
      const ready = await Promise.all(markers.map((marker) => access(marker).then(() => true, () => false)));
      return ready.every(Boolean);
    }, { timeout: 45_000, timeoutMsg: "The fixture jobs did not fill the execution capacity" });

    const { chat, job } = await enqueue("Detached queued job", "Run the queued job while detached");
    const blocked = await queue();
    assert.equal(blocked.active, capacity);
    assert.ok(holders.every((holder) =>
      blocked.jobs.find((entry) => entry.job.id === holder.id)?.job.latestAttempt?.status === "running"
    ));
    const waiting = blocked.jobs.find((entry) => entry.job.id === job.id);
    assert.deepEqual(waiting.blockers, [{ type: "capacity", active: capacity, limit: capacity }]);
    assert.equal(waiting.job.latestAttempt, null);
    assert.equal(await invoke(browser, "latest_conversation_run", { conversationId: chat.id }), null);

    await browser.$('button[aria-label="Queue"]').click();
    const queuePanel = browser.$('section[aria-label="Execution queue"]');
    await queuePanel.$("button=Pause queue").waitForEnabled();
    pausedByTest = true;
    await queuePanel.$("button=Pause queue").click();
    await browser.waitUntil(async () => (await queue()).settings.paused);
    assert.equal((await queue()).active, capacity);
    await queuePanel.$("button=Resume queue").waitForEnabled();
    await queuePanel.$("button=Resume queue").click();
    await browser.waitUntil(async () => !(await queue()).settings.paused);
    pausedByTest = false;

    const waitingRow = queuePanel.$('.//tr[td[contains(., "Waiting for an execution slot")]]');
    await waitingRow.$("button=Conversation turn").waitForDisplayed();
    await waitingRow.$("button=Conversation turn").click();
    const pending = async () => (await queue()).jobs.find((entry) => entry.job.id === job.id);
    await queuePanel.$("button=Pause job").click();
    await browser.waitUntil(async () => (await pending()).job.queueState === "paused");
    assert.ok((await pending()).blockers.some((blocker) => blocker.type === "job_paused"));
    await queuePanel.$("button=Resume job").waitForEnabled();
    await queuePanel.$("button=Resume job").click();
    await browser.waitUntil(async () => (await pending()).job.queueState === "queued");
    assert.deepEqual((await pending()).blockers, [{ type: "capacity", active: capacity, limit: capacity }]);
    await queuePanel.$("button=Open conversation").click();

    async function openConversation() {
      await chooseConversations(browser);
      await clickConversation(browser, chat.title);
      return browser.$(`section[aria-label="${chat.title}"]`);
    }
    const before = browser.$(`section[aria-label="${chat.title}"]`);
    await before.$(`p=Waiting for an execution slot (${capacity} of ${capacity} in use).`).waitForDisplayed();

    const visible = () => invoke(browser, "plugin:window|is_visible", { label: "main" });
    await invoke(browser, "plugin:window|close", { label: "main" });
    await browser.waitUntil(async () => !(await visible()));
    detached = true;
    await browser.url("about:blank");
    assert.equal(await browser.getUrl(), "about:blank");
    assert.equal(await browser.execute(() => document.getElementById("root") === null), true);
    await assert.rejects(access(join(repo, "detached-job-dispatched")), { code: "ENOENT" });

    // After unloading the application's document, use the test fixture's file gate and
    // committed SQLite rows as witnesses. No app IPC or UI subscriber drives this job.
    await writeFile(release, "");
    const database = new DatabaseSync(join(fixtureProfile(application), "omniagent.sqlite3"), { readOnly: true });
    let runtime;
    try {
      const attempt = database.prepare(`
        SELECT attempts.run_id AS runId, attempts.ordinal, runs.status
        FROM job_attempts AS attempts JOIN runs ON runs.id = attempts.run_id
        WHERE attempts.job_id = ? ORDER BY attempts.ordinal DESC LIMIT 1
      `);
      await browser.waitUntil(() => {
        runtime = attempt.get(job.id);
        return ["succeeded", "failed", "cancelled", "interrupted"].includes(runtime?.status);
      }, { timeout: 45_000, timeoutMsg: "No terminal result was committed with the WebView detached" });
    } finally {
      database.close();
    }
    assert.equal(runtime.status, "succeeded");
    assert.equal(runtime.ordinal, 1);
    await access(join(repo, "detached-job-dispatched"));
    assert.equal(await browser.getUrl(), "about:blank");

    await browser.url(page);
    detached = false;
    await waitForRuntime(browser);
    await showTestApplication(browser, application);
    const restored = await queue();
    const completed = restored.jobs.find((entry) => entry.job.id === job.id);
    assert.deepEqual(completed.blockers, []);
    assert.equal(completed.job.latestAttempt.runId, runtime.runId);
    assert.equal(completed.job.latestAttempt.ordinal, runtime.ordinal);
    assert.equal(completed.job.latestAttempt.status, runtime.status);
    const after = await openConversation();
    await after.$("p=The queued job ran with the WebView detached.").waitForDisplayed();
    await click(after, 'button=Activity');
    const activity = await checkConversationActivity(browser);
    const status = activity.$('.run-inspection-header [role="status"]');
    await status.waitForDisplayed();
    assert.equal(await status.getText(), "Completed");
    assert.equal(await browser.execute(() => {
      const workspace = document.querySelector('.work-deck:not([hidden]) .work-view:not([hidden])');
      const entries = workspace.querySelector('.conv-library-entries');
      const transcript = workspace.querySelector('.conv-workspace-main');
      return entries ? entries.scrollWidth <= entries.clientWidth + 1 : transcript.clientWidth >= Math.min(600, workspace.clientWidth);
    }), true, "Activity must leave room for the conversation, with the library available when there is space");
    await click(activity.$('[aria-label="Run details"]'), 'button=Artifacts');
    await activity.$('h3=Recorded output').waitForDisplayed();
    await capture(browser, "10-activity-real");
    await click(activity, 'button[aria-label="Close activity"]');
    await browser.$('.work-deck:not([hidden]) .conv-library-entries').waitForDisplayed();
    await click(after, '.conv-composer .model-picker-trigger');
    const modelMenu = browser.$('.model-picker-menu[data-catalog]');
    await modelMenu.$('.model-capability-icons [title^="CLI inventory"]').waitForDisplayed();
    await capture(browser, "real-model-catalog-details");
    await click(browser, `.model-picker-menu [role="menuitemradio"][aria-label=${JSON.stringify(fixture.option.name)}]`);
    const composerBounds = () => browser.execute((title) => {
      const form = [...document.querySelectorAll('.conv-pane')].find((pane) => pane.getAttribute("aria-label") === title)?.querySelector('.conv-composer');
      return [form?.clientHeight, form?.scrollHeight, form?.scrollTop];
    }, chat.title);
    const priorBounds = await composerBounds();
    await click(after, ".conv-compaction-trigger");
    const compaction = browser.$('[role="dialog"][aria-label="Context compaction settings"]');
    await compaction.waitForDisplayed();
    assert.deepEqual(await composerBounds(), priorBounds);
    await capture(browser, "real-compact-context");
    await browser.keys("Escape");
    await compaction.waitForDisplayed({ reverse: true });
    await click(browser, '.work-rail button[aria-label="Settings"]');
    const settings = browser.$('.settings-dialog');
    await click(settings.$('[aria-label="Settings categories"]'), "button=System");
    await click(settings, "summary=Qualification matrix");
    const matrix = '.settings-dialog table[aria-label="Qualification matrix"]';
    await browser.$(matrix).waitForDisplayed({ timeout: 10_000 });
    await browser.waitUntil(() => browser.execute((selector) => Array.from(document.querySelectorAll(selector),
      (cell) => cell.textContent).some((model) => model.includes("desktop-fixture")), `${matrix} tbody td:nth-child(2)`),
    { timeoutMsg: "Qualification matrix should show the model from the persisted dispatch" });
    await capture(browser, "real-settings-qualification");
    await click(settings, 'button[aria-label="Close settings"]');

    for (const [width, height, drawer] of [[1440, 900, false], [1120, 440, true], [640, 440, true]]) {
      await invoke(browser, "plugin:window|set_size", { label: "main", value: { Logical: { width, height } } });
      await browser.waitUntil(() => browser.execute((size) => Math.abs(innerWidth - size) <= 4, width));
      const queueButton = '.work-rail button[aria-label="Queue"]';
      if (await browser.$(queueButton).getAttribute("aria-pressed") !== "true") await click(browser, queueButton);
      const panel = browser.$('.work-queue-panel:not([hidden])');
      await click(panel, `button[data-queue-job="${job.id}"]`);
      assert.equal(await panel.getAttribute("role") === "dialog", drawer);
      await click(panel, "button=Inspect attempt");
      const inspection = await checkConversationActivity(browser);
      await inspection.$('.run-inspection-header [role="status"]').waitForDisplayed();
      if (drawer) await panel.waitForDisplayed({ reverse: true });
      await click(inspection, 'button[aria-label="Close activity"]');
      await panel.$("button=Inspect attempt").waitForDisplayed();
      await browser.waitUntil(() => browser.execute(() => document.activeElement?.textContent?.trim() === "Inspect attempt"));
      await capture(browser, `real-queue-return-${width}x${height}`);
      await click(panel, 'button[aria-label="Close queue"]');
    }
    await invoke(browser, "plugin:window|set_size", { label: "main", value: { Logical: { width: 1120, height: 760 } } });
  } finally {
    await writeFile(release, "");
    if (detached) {
      await browser.url(page);
      await waitForRuntime(browser);
    }
    if (pausedByTest) await invoke(browser, "set_job_queue_paused", { paused: false });
    await invoke(browser, "set_cli_executable", { provider: "codex", executable: profile.executable });
  }
}

async function verifyPendingInstructions(browser, application, scratch) {
  const repo = join(scratch, "Pending instruction checkout");
  git(scratch, "init", "-q", repo);
  git(repo, "-c", "user.name=OmniAgent Fixture", "-c", "user.email=fixture@example.invalid",
    "-c", "commit.gpgsign=false", "commit", "-q", "--allow-empty", "-m", "Fixture");
  const checkout = await invoke(browser, "register_project", { directory: repo });
  await selectCheckout(browser, checkout);
  const profile = (await invoke(browser, "cli_executables")).find(({ provider }) => provider === "codex");
  assert.ok(profile);
  await invoke(browser, "set_cli_executable", { provider: "codex", executable: application });
  const release = join(repo, "release-desktop-jobs");
  try {
    const models = await invoke(browser, "conversation_models", { provider: "codex" });
    const fixture = models.find(({ option }) => option.selection.modelId === "desktop-fixture" && option.selectable);
    assert.ok(fixture);
    const title = "Edit pending instructions";
    const chat = await invoke(browser, "create_conversation", { checkoutId: checkout.checkoutId, title });
    const scope = { conversationId: chat.id };
    const text = "Hold desktop slot 1";
    await invoke(browser, "save_conversation_draft", { ...scope, text });
    await invoke(browser, "start_conversation_turn", { ...scope, selection: fixture.option.selection, mode: "read_only", text });
    const open = async () => {
      await chooseConversations(browser);
      await clickConversation(browser, title);
      return browser.$(`section[aria-label="${title}"]`);
    };
    const panel = await open();
    await panel.$("button=Stop").waitForDisplayed();
    await browser.waitUntil(() => access(join(repo, "desktop-slot-1")).then(() => true, () => false),
      { timeout: 30_000, timeoutMsg: "The active turn did not reach the CLI" });
    await panel.$(".model-picker-trigger").click();
    await click(browser, `.model-picker-menu [role="menuitemradio"][aria-label="${fixture.option.name}"]`);
    const pending = () => invoke(browser, "pending_conversation_instructions", scope);
    const instruction = (text) => panel.$(`.//li[p[text()="${text}"]]`);
    const draft = panel.$("textarea[id^=draft-]");
    const obsolete = "Replace this queued instruction";
    const removed = "Remove this queued instruction";
    for (const text of [obsolete, removed]) {
      await draft.setValue(text);
      const queue = panel.$("button=Queue for next turn");
      await queue.waitForEnabled();
      await queue.click();
      await instruction(text).waitForDisplayed();
      await draft.waitForEnabled();
    }
    const original = await pending();
    assert.deepEqual(original.map(({ text }) => text), [obsolete, removed]);
    assert.ok(original.every(({ checkoutId, provider, model }) =>
      checkoutId === checkout.checkoutId && provider === "codex" && model === "desktop-fixture"));

    await browser.refresh();
    await waitForRuntime(browser);
    await open();
    assert.deepEqual((await pending()).map(({ id, contextId }) => [id, contextId]),
      original.map(({ id, contextId }) => [id, contextId]));
    await instruction(obsolete).$("button=Edit instruction").click();
    const edited = "Run the revised queued instruction";
    await panel.$('textarea[aria-label="Pending instruction text"]').setValue(edited);
    await panel.$("button=Save instruction").click();
    await instruction(edited).waitForDisplayed();
    await instruction(removed).$("button=Remove instruction").click();
    await instruction(removed).waitForExist({ reverse: true });
    const [updated] = await pending();
    assert.equal(updated.id, original[0].id);
    assert.notEqual(updated.revisionId, original[0].revisionId);
    assert.equal(updated.contextId, original[0].contextId);
    await writeFile(release, "");
    await panel.$("p=The revised instruction ran.").waitForDisplayed({ timeout: 30_000 });
    assert.equal(await readFile(join(repo, "queued-instruction-dispatched"), "utf8"), edited);
    const messages = await invoke(browser, "conversation_messages", scope);
    assert.deepEqual(messages.filter(({ role }) => role === "user").map(({ text }) => text), [text, edited]);
    assert.equal(messages.find(({ text }) => text === edited).revisionId, updated.revisionId);

    await browser.waitUntil(async () => (await invoke(browser, "latest_conversation_run", scope))?.status === "succeeded",
      { timeout: 30_000 });
    assert.equal(await panel.$('summary=Instruction delivery history').isExisting(), false);
    await click(panel, 'button[aria-label="Instruction delivery history"]');
    const deliveries = browser.$('.instruction-history-dialog[role="dialog"]');
    await deliveries.$('[aria-label="Recorded instruction deliveries"]').waitForDisplayed();
    const revisedDelivery = deliveries.$(`.//li[.//p[normalize-space(.)="${edited}"]]`);
    await revisedDelivery.waitForDisplayed();
    await capture(browser, "real-instruction-delivery-history");
    await click(revisedDelivery, "button*=Open");
    await deliveries.waitForDisplayed({ reverse: true });
    const receivingTurn = await checkConversationActivity(browser);
    await click(receivingTurn, 'button[aria-label="Close activity"]');
    const current = await invoke(browser, "conversation_history", scope);
    const oldEmptyContext = await invoke(browser, "rewind_conversation_history", {
      ...scope, baseId: current.contextId, throughRevision: null,
    });
    await invoke(browser, "rewind_conversation_history", {
      ...scope, baseId: oldEmptyContext, throughRevision: null,
    });
    const stale = "Keep the draft from the earlier empty history";
    await invoke(browser, "save_conversation_draft", { ...scope, text: stale });
    await assert.rejects(invoke(browser, "start_conversation_turn", {
      ...scope, selection: fixture.option.selection, mode: "read_only", text: stale, expectedContextId: oldEmptyContext,
    }), /conversation context changed/i);
    assert.equal(await invoke(browser, "conversation_draft", scope), stale);
    assert.deepEqual(await pending(), []);
  } finally {
    await writeFile(release, "");
    await invoke(browser, "set_cli_executable", { provider: "codex", executable: profile.executable });
  }
}

async function verifyQuestionContinuation(browser, application, scratch) {
  const repo = join(scratch, "Question continuation checkout");
  git(scratch, "init", "-q", repo);
  git(repo, "-c", "user.name=OmniAgent Fixture", "-c", "user.email=fixture@example.invalid",
    "-c", "commit.gpgsign=false", "commit", "-q", "--allow-empty", "-m", "Fixture");
  const checkout = await invoke(browser, "register_project", { directory: repo });
  const project = { projectId: checkout.projectId };
  await selectCheckout(browser, checkout);
  const page = await browser.getUrl();
  const { settings } = await invoke(browser, "project_job_queue", project);
  const profile = (await invoke(browser, "cli_executables")).find(({ provider }) => provider === "codex");
  assert.ok(profile);
  await invoke(browser, "set_cli_executable", { provider: "codex", executable: application });
  await invoke(browser, "set_job_queue_capacity", { capacity: 1 });
  let detached = false;
  try {
    const models = await invoke(browser, "conversation_models", { provider: "codex" });
    const fixture = models.find(({ option }) => option.selection.modelId === "desktop-fixture" && option.selectable);
    assert.ok(fixture);
    const submit = async (title, text, mode = "plan") => {
      const chat = await invoke(browser, "create_conversation", { checkoutId: checkout.checkoutId, title });
      const scope = { conversationId: chat.id };
      await invoke(browser, "save_conversation_draft", { ...scope, text });
      const job = await invoke(browser, "start_conversation_turn", { ...scope, selection: fixture.option.selection, mode, text });
      return { title, scope, job };
    };
    const latest = ({ scope }) => invoke(browser, "latest_conversation_run", scope);
    const open = async ({ title }) => {
      await chooseConversations(browser);
      await clickConversation(browser, title);
      return browser.$(`section[aria-label="${title}"]`);
    };
    const reopen = async () => {
      const visible = () => invoke(browser, "plugin:window|is_visible", { label: "main" });
      await invoke(browser, "plugin:window|close", { label: "main" });
      await browser.waitUntil(async () => !(await visible()));
      detached = true;
      await browser.url("about:blank");
      assert.equal(await browser.getUrl(), "about:blank");
      await browser.url(page);
      await waitForRuntime(browser);
      await showTestApplication(browser, application);
      detached = false;
    };
    const waitForQuestion = async (chat) => {
      let run;
      await browser.waitUntil(async () => {
        run = await latest(chat);
        return run?.status === "waiting_for_input";
      }, { timeout: 30_000, timeoutMsg: "The CLI did not open its native question" });
      return { ...chat.scope, runId: run.id };
    };
    const reply = async (card, value) => {
      await click(card, `.//label[input[@type="radio"] and contains(normalize-space(.), ${JSON.stringify(value)})]`);
      await click(card, "button=Reply");
    };

    const live = await submit("Live question across window close", "Ask desktop question live");
    const liveOrigin = await waitForQuestion(live);
    await reopen();
    assert.equal((await latest(live)).id, liveOrigin.runId);
    const livePanel = await open(live);
    await reply(livePanel.$('[aria-label="Pending requests"] [data-question-request="71"]'), "Markdown");
    await livePanel.$("p=The live turn received Markdown.").waitForDisplayed({ timeout: 30_000 });
    await assert.rejects(invoke(browser, "answer_conversation_question", {
      ...liveOrigin, requestId: 71, answer: { action: "accept", values: { format: "Markdown" } },
    }));
    assert.equal((await readFile(join(repo, "desktop-live-question-reply"), "utf8")).trim(), "Markdown");
    assert.equal((await latest(live)).id, liveOrigin.runId);

    assert.equal(await invoke(browser, "project_question_timeout", project), 10);
    for (const [kind, minutes] of [["default", 10], ["custom", 2]]) {
      if (kind === "custom") await invoke(browser, "set_project_question_timeout", { ...project, minutes });
      const source = await submit(`Expiring ${kind} planning question`, `Ask desktop question ${kind}`);
      const origin = await waitForQuestion(source);
      const probe = await submit(`Slot probe after ${kind}`, "Complete desktop slot probe", "read_only");
      const queue = await invoke(browser, "project_job_queue", project);
      assert.equal(queue.active, 1);
      assert.deepEqual(queue.jobs.find(({ job }) => job.id === probe.job.id).blockers,
        [{ type: "capacity", active: 1, limit: 1 }]);

      // Advance the first request's stored wall-clock timestamps. A second provider
      // request makes the running supervisor re-read both deadlines.
      const db = new DatabaseSync(join(fixtureProfile(application), "omniagent.sqlite3"));
      try {
        db.exec("PRAGMA busy_timeout = 5000");
        const deadline = db.prepare(`SELECT w.request_event_seq AS seq, deadline_ms - opened_ms AS duration
          FROM native_question_waits w JOIN agent_questions q USING (request_event_seq) WHERE q.run_id = ?`).get(origin.runId);
        assert.equal(deadline.duration, minutes * 60_000);
        assert.equal(db.prepare(`UPDATE native_question_waits SET opened_ms = opened_ms - ?, deadline_ms = deadline_ms - ?
          WHERE request_event_seq = ? AND state = 'live'`).run(deadline.duration, deadline.duration, deadline.seq).changes, 1);
      } finally {
        db.close();
      }
      await writeFile(join(repo, `advance-${kind}-question`), "");
      await browser.waitUntil(async () => (await latest(probe))?.status === "succeeded",
        { timeout: 30_000, timeoutMsg: "A queued turn could not use the expired question's slot" });
      await reopen();
      const saved = await invoke(browser, "saved_conversation_questions", origin);
      assert.deepEqual(saved.map(({ event, state, reason }) => [event.data.request_id, state, reason]),
        [[71, "saved", "timeout"], [72, "saved", "turn_stopped"]]);
      assert.equal((await latest(source)).id, origin.runId);
      const panel = await open(source);
      const format = panel.$('[data-question-request="71"]');
      await reply(format, "Markdown");
      await panel.$("p=Answer recorded").waitForDisplayed();
      assert.equal((await latest(source)).id, origin.runId);
      const summary = panel.$('[data-question-request="72"]');
      await reply(summary, "Yes");
      await panel.$("p=The linked planning turn received both answers.").waitForDisplayed({ timeout: 30_000 });
      const followup = (await invoke(browser, "latest_conversation_job", source.scope)).job.latestAttempt;
      assert.equal(followup.previousRunId, origin.runId);
      assert.equal(followup.ordinal, 2);
      await assert.rejects(invoke(browser, "answer_saved_conversation_question", {
        ...origin, requestId: 71, answer: { action: "accept", values: { format: "Markdown" } },
      }));
    }
  } finally {
    await Promise.all(["default", "custom"].map((kind) => writeFile(join(repo, `advance-${kind}-question`), "")));
    if (detached) {
      await browser.url(page);
      await waitForRuntime(browser);
      await showTestApplication(browser, application);
    }
    await invoke(browser, "set_job_queue_capacity", { capacity: settings.capacity });
    await invoke(browser, "set_cli_executable", { provider: "codex", executable: profile.executable });
  }
}

async function verifyJobPolicy(browser, application, scratch) {
  const repo = join(scratch, "Policy checkout");
  git(scratch, "init", "-q", repo);
  git(repo, "-c", "user.name=OmniAgent Fixture", "-c", "user.email=fixture@example.invalid",
    "-c", "commit.gpgsign=false", "commit", "-q", "--allow-empty", "-m", "Fixture");
  const checkout = await invoke(browser, "register_project", { directory: repo });
  await selectCheckout(browser, checkout);
  await writeFile(join(repo, "release-desktop-jobs"), "");
  const projectId = checkout.projectId;
  const queue = () => invoke(browser, "project_job_queue", { projectId });
  const configured = (await invoke(browser, "cli_executables")).find(({ provider }) => provider === "codex");
  const initial = (await queue()).settings;
  const originalRules = await invoke(browser, "project_job_rules", { projectId });
  await invoke(browser, "set_cli_executable", { provider: "codex", executable: application });
  let rules = originalRules;
  async function setRules(next) {
    rules = await invoke(browser, "set_project_job_rules", { projectId, expected: rules, rules: next });
  }

  try {
    assert.equal(initial.capacity, 3);
    await browser.$('button[aria-label="Queue"]').click();
    const queuePanel = browser.$('section[aria-label="Execution queue"]');
    await queuePanel.$("summary=Queue settings").click();
    await queuePanel.$('input[type="number"]').setValue(2);
    await queuePanel.$("button=Save capacity").click();
    await browser.waitUntil(async () => (await queue()).settings.capacity === 2);

    const allowed = {
      role: "operator",
      operation: "read_only",
      workspace: { type: "checkout", checkoutId: checkout.checkoutId },
      permission: "allow",
      requirements: {
        capabilities: ["native_goal_command"],
        customizations: [{ type: "instruction", path: "AGENTS.md" }],
      },
    };
    await setRules([allowed]);
    const models = await invoke(browser, "conversation_models", { provider: "codex" });
    const model = models.find(({ option }) => option.selection.modelId === "desktop-fixture" && option.selectable);
    assert.ok(model);
    const chat = await invoke(browser, "create_conversation", { checkoutId: checkout.checkoutId, title: "Policy check" });
    const text = "Run the queued job while detached";
    await invoke(browser, "save_conversation_draft", { conversationId: chat.id, text });
    const job = await invoke(browser, "start_conversation_turn", {
      conversationId: chat.id, selection: model.option.selection, mode: "read_only", text,
    });
    const inspect = async () => (await queue()).jobs.find((entry) => entry.job.id === job.id);
    await browser.waitUntil(async () => (await inspect()).job.latestAttempt?.status === "failed", { timeout: 20_000 });
    const failed = await inspect();
    const runId = failed.job.latestAttempt.runId;
    assert.deepEqual(failed.blockers, [{ type: "dispatch_rejected", rejection: {
      type: "requirements", ...allowed.requirements,
    } }]);
    assert.equal(await invoke(browser, "conversation_input", { conversationId: chat.id, runId }), null);
    await assert.rejects(access(join(repo, "detached-job-dispatched")), { code: "ENOENT" });

    await writeFile(join(repo, "AGENTS.md"), "# Fixture\n\nUse the selected worktree.\n");
    await setRules([{ ...allowed, requirements: { ...allowed.requirements, capabilities: [] } }]);
    const alternate = join(scratch, "refreshed-cli");
    await mkdir(alternate);
    const executable = join(alternate, "omniagent.exe");
    await copyFile(application, executable);
    await invoke(browser, "set_cli_executable", { provider: "codex", executable });
    await invoke(browser, "queue_job_recovery", { projectId, jobId: job.id, runId, action: "retry" });
    await browser.waitUntil(async () => {
      const attempt = (await inspect()).job.latestAttempt;
      return attempt?.previousRunId === runId && ["succeeded", "failed", "cancelled", "interrupted"].includes(attempt.status);
    }, { timeout: 20_000 });
    const retried = await inspect();
    assert.deepEqual(retried.blockers, []);
    assert.equal(retried.job.latestAttempt.status, "succeeded");
    await access(join(repo, "detached-job-dispatched"));
  } finally {
    await setRules(originalRules);
    await invoke(browser, "set_job_queue_capacity", { capacity: initial.capacity });
    await invoke(browser, "set_cli_executable", { provider: "codex", executable: configured.executable });
  }
}

async function copyTestApplication(t) {
  assert.equal(process.platform, "win32");
  const root = await realpath(tmpdir());
  const runId = randomUUID().replaceAll("-", "");
  const scratch = await mkdtemp(join(root, `omniagent-webdriver-${runId}-`));
  const application = join(scratch, "omniagent.exe");
  await assert.rejects(access(fixtureProfile(application)), { code: "ENOENT" });
  t.after(async () => {
    await removeFixtureProfile(application);
    assert.equal(dirname(await realpath(scratch)), root);
    await rm(scratch, { recursive: true, force: true, maxRetries: 4, retryDelay: 500 });
  });
  await copyFile(resolve(process.env.OMNIAGENT_TEST_APP ?? "src-tauri/target/debug/omniagent.exe"), application);
  return { application, scratch };
}

function startTestDriver(application) {
  const args = ["--port", String(port), "--native-port", String(port + 1)];
  if (process.env.OMNIAGENT_EDGE_DRIVER) args.push("--native-driver", process.env.OMNIAGENT_EDGE_DRIVER);
  return spawn(process.env.OMNIAGENT_TAURI_DRIVER ?? "tauri-driver", args, {
    windowsHide: true, stdio: "ignore", env: fixtureEnvironment(application),
  });
}

async function connectTestDriver(driver, application) {
  await Promise.race([
    waitForDriver(driver),
    new Promise((_, reject) => driver.once("error", reject)),
  ]);
  return startSession(application);
}

test("runs fixtures in the Windows desktop", { timeout: 240_000 }, async (t) => {
  const { application, scratch } = await copyTestApplication(t);
  const repo = join(scratch, "Repositório com espaço");
  const linked = join(scratch, "Árvore linked");
  git(scratch, "init", "-q", repo);
  git(repo, "-c", "user.name=OmniAgent Fixture", "-c", "user.email=fixture@example.invalid",
    "-c", "commit.gpgsign=false", "commit", "-q", "--allow-empty", "-m", "Fixture");
  git(repo, "worktree", "add", "-q", "--detach", linked);
  await writeFile(join(repo, "AGENTS.md"), "Desktop registration keeps Markdown available.");
  await mkdir(join(repo, ".claude"));
  await mkdir(join(repo, "scripts"));
  await writeFile(join(repo, "scripts", "startup.ps1"), "Set-Content hook-marker started");
  await writeFile(join(repo, "scripts", "mcp.js"), "require('node:fs').writeFileSync('mcp-marker', 'started');");
  await writeFile(join(repo, ".claude", "settings.json"), JSON.stringify({
    hooks: { SessionStart: [{ matcher: "startup", hooks: [{ type: "command", command: "powershell.exe -NoProfile -File ./scripts/startup.ps1" }] }] },
  }));
  await writeFile(join(repo, ".mcp.json"), JSON.stringify({ mcpServers: { fixture: { command: "node", args: ["./scripts/mcp.js"] } } }));

  const driver = startTestDriver(application);
  let browser;
  try {
    browser = await connectTestDriver(driver, application);
    await browser.$("button=Open repository").waitForDisplayed();
    await assert.rejects(invoke(browser, "register_project", { directory: scratch }), /git repository/i);
    const main = await invoke(browser, "register_project", { directory: repo });
    const checkout = await invoke(browser, "register_project", { directory: linked });
    const registered = await invoke(browser, "registered_checkouts");
    assert.ok(registered.some((item) => item.checkoutId === main.checkoutId));
    assert.ok(registered.some((item) => item.checkoutId === checkout.checkoutId));
    assert.equal(main.projectId, checkout.projectId);
    assert.notEqual(main.checkoutId, checkout.checkoutId);
    assert.equal(await realpath(checkout.checkoutRoot), await realpath(linked));
    await selectCheckout(browser, checkout);
    await browser.$('button[aria-label="Diagnostics"]').click();
    await browser.$("h2=Current installation").waitForDisplayed();
    await browser.$("button=System settings").click();
    await browser.$('.settings-dialog [data-setting="environment"]').waitForDisplayed();
    await browser.keys("Escape");
    await browser.waitUntil(() => browser.execute(() => document.activeElement?.textContent?.trim() === "System settings"));
    await browser.$("button=Fixture runs").click();
    await browser.$("button=Start fixture").waitForDisplayed();

    await browser.$("button=Start fixture").click();
    const input = await browser.$("form input");
    await input.waitForDisplayed();
    await input.setValue("Entrada de validação");
    await click(browser, "button=Desktop");
    await click(browser, "button=Fixture runs");
    await input.waitForDisplayed();
    assert.equal(await input.getValue(), "Entrada de validação");
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
    await browser.$("button=Fixture runs").click();
    await browser.$("button=Start fixture").click();
    await browser.$("button=Cancel run").waitForDisplayed();
    const secondHeading = await browser.$(".dv-content-container h1").getText();
    assert.notEqual(firstHeading, secondHeading);
    await browser.$("button=Cancel run").click();
    await browser.waitUntil(async () => (await events().getText()).includes("Cancelled"));
    await browser.$('button[aria-label="Split right"]').click();
    const expectedTabs = { groups: [[firstHeading, "Fixture runs"], [secondHeading]], active: secondHeading };
    await browser.waitUntil(async () => (await readTabs(browser)).groups.length === 2);
    assert.deepEqual(await readTabs(browser), expectedTabs);
    await browser.waitUntil(async () => {
      const saved = await invoke(browser, "workbench_layout", { checkoutId: checkout.checkoutId, workspace: "diagnostics" });
      return saved && JSON.parse(saved).grid.root.data.length === 2;
    });

    await browser.deleteSession();
    browser = null;
    browser = await startSession(application);
    await click(browser, 'button[aria-label="Diagnostics"]');
    await click(browser, "button=Fixture runs");
    await browser.waitUntil(async () => (await readTabs(browser)).groups.length === 2);
    assert.deepEqual(await readTabs(browser), expectedTabs);
    assert.equal(await browser.$("header span[title]").getAttribute("title"), checkout.checkoutRoot);

    await browser.$("button=Fixture runs").click();
    await browser.$("button=Start fixture").click();
    await browser.$("form input").waitForDisplayed();
    const scope = { checkoutId: checkout.checkoutId };
    const waiting = await invoke(browser, "run_snapshot", scope);
    const runId = waiting.runs.find((run) => run.status === "waiting_for_input").id;
    const visible = () => invoke(browser, "plugin:window|is_visible", { label: "main" });
    assert.equal(await visible(), true);
    await invoke(browser, "plugin:window|close", { label: "main" });
    await browser.waitUntil(async () => !(await visible()));

    await invoke(browser, "send_fixture_input", { ...scope, runId, input: "Sent while hidden" });
    await browser.waitUntil(async () => {
      const snapshot = await invoke(browser, "run_snapshot", scope);
      return snapshot.runs.find((run) => run.id === runId).status === "succeeded";
    });
    // Starting the same executable forwards to the visible window in the
    // existing single instance. The attached WebDriver session keeps its page.
    await showTestApplication(browser, application);
    await browser.waitUntil(async () => (await runRows(browser, runId)).includes("Succeeded"));
    const history = (await invoke(browser, "run_snapshot", scope)).recentEvents.filter((event) => event.runId === runId);
    assert.equal(new Set(history.map((event) => event.seq)).size, history.length);
    assert.ok(history.every((event, index) => index === 0 || BigInt(history[index - 1].seq) < BigInt(event.seq)));
    assert.equal((await runRows(browser, runId)).length, history.length);

    await selectCheckout(browser, main);
    await browser.$('button[aria-label="Settings"]').click();
    await browser.$("button=Customizations").click();
    const reviewScope = { checkoutId: main.checkoutId };
    const initial = await invoke(browser, "review_checkout_customizations", reviewScope);
    const approvedHook = initial.entries.find(({ kind }) => kind === "hook");
    assert.ok(approvedHook);
    const hook = browser.$('//details[summary[contains(., "SessionStart")]]');
    await hook.waitForDisplayed();
    const hookSummary = hook.$("summary");
    assert.match(await hookSummary.getText(), /Pending review/);
    const markdown = browser.$("summary=AGENTS.md");
    await markdown.click();
    await browser.$("pre=Desktop registration keeps Markdown available.").waitForDisplayed();
    await hookSummary.click();
    await hook.$("button=Approve revision").click();
    await browser.waitUntil(async () => (await hookSummary.getText()).includes("Revision approved"));
    assert.match(await browser.$("summary*=fixture").getText(), /Pending review/);
    await writeFile(join(repo, "scripts", "startup.ps1"), "Set-Content hook-marker changed");
    await assert.rejects(invoke(browser, "approve_checkout_customization", {
      ...reviewScope, entryId: approvedHook.id, fingerprint: approvedHook.fingerprint,
    }), /Customization changed/);
    await browser.$("button=Refresh").click();
    await browser.waitUntil(async () => (await hookSummary.getText()).includes("Changed since approval"));
    await assert.rejects(access(join(repo, "hook-marker")), { code: "ENOENT" });
    await assert.rejects(access(join(repo, "mcp-marker")), { code: "ENOENT" });

    const settingsBack = browser.$("button.settings-back");
    if (await settingsBack.isDisplayed()) await settingsBack.click();
    const settingsNav = browser.$('.settings-dialog nav[aria-label="Settings categories"]');
    await click(settingsNav, './/button[span[normalize-space(.)="Providers"]]');
    const executable = browser.$('input[name="codex"]');
    await executable.waitForDisplayed();
    await executable.setValue(application);
    const form = browser.$('form:has(input[name="codex"])');
    await form.$("button=Save").click();
    await browser.waitUntil(async () => {
      const providers = await invoke(browser, "cli_executables");
      return providers.find((provider) => provider.provider === "codex")?.executable === application;
    });
    await form.$("button=Use PATH").click();
    await browser.waitUntil(async () => {
      const providers = await invoke(browser, "cli_executables");
      return providers.find((provider) => provider.provider === "codex")?.executable === null;
    });
    if (process.env.OMNIAGENT_LIVE_PLUGIN_INVENTORY === "1") {
      await form.$("button=Check plugins").click();
      const summary = browser.$("summary*=Installed plugins");
      await summary.waitForDisplayed({ timeout: 45_000 });
      const inventory = await invoke(browser, "cli_plugin_inventory", { provider: "codex", refresh: false });
      assert.equal(inventory.report.status, "reported");
      assert.ok(inventory.report.version);
      assert.equal(await summary.getText(), `Installed plugins (${inventory.report.plugins.length})`);
      await summary.click();
      await browser.$("p*=In-session plugin loads are unconfirmed").waitForDisplayed();
    }

    await browser.$('button[aria-label="Close settings"]').click();
    await chooseConversations(browser);
    await browser.$('button[aria-label="New conversation"]').click();
    const newConversation = browser.$('.conversation-pipeline-dialog[role="dialog"]');
    await newConversation.$("h2=Choose a conversation pipeline").waitForDisplayed();
    assert.equal(await newConversation.$("button=Create conversation").isEnabled(), false);
    await capture(browser, "real-conversation-pipeline-picker");
    await click(newConversation, './/label[.//strong[normalize-space(.)="Direct conversation"]]');
    await click(newConversation, "button=Create conversation");
    const draft = browser.$('textarea[id^="draft-"]');
    await draft.waitForEnabled();
    const unsent = "Rascunho — revisar depois ☕";
    await draft.setValue(unsent);
    await browser.$("p=Draft saved").waitForDisplayed();
    const [conversation] = await invoke(browser, "list_conversations", { checkoutId: main.checkoutId });
    assert.equal(conversation.pipeline?.kind, "direct");
    assert.equal(await invoke(browser, "conversation_draft", { conversationId: conversation.id }), unsent);
    assert.deepEqual(await invoke(browser, "conversation_messages", { conversationId: conversation.id }), []);
    await click(browser, 'button[aria-label="Settings"]');
    await click(browser, "button=Conversation defaults");
    const projectHistoryPolicy = browser.$('select[id="queued-context"]');
    await projectHistoryPolicy.waitForEnabled();
    assert.equal(await projectHistoryPolicy.getValue(), "review");
    await projectHistoryPolicy.selectByAttribute("value", "recorded");
    await browser.waitUntil(async () => (await invoke(browser, "project_queue_history_policy", { projectId: main.projectId })) === "recorded");
    await browser.keys("Escape");
    await click(browser, "button=History");
    const historyDialog = browser.$('.conv-dialog[role="dialog"]');
    assert.equal(await historyDialog.$('[aria-label="Next prompt scope"]').isExisting(), false);
    await capture(browser, "real-conversation-history");
    await click(historyDialog, "summary=Options");
    const conversationHistoryPolicy = historyDialog.$('select:has(option[value="review"])');
    await conversationHistoryPolicy.waitForEnabled();
    assert.ok((await conversationHistoryPolicy.$('option[value=""]').getText()).includes("Use recorded context"));
    await conversationHistoryPolicy.selectByAttribute("value", "cancel");
    await click(browser, "button=Confirm clear");
    await historyDialog.waitForExist({ reverse: true });
    const cleared = await invoke(browser, "conversation_history", { conversationId: conversation.id });
    assert.notEqual(cleared.contextId, conversation.contextId);
    assert.deepEqual(cleared.messages, []);
    assert.equal(await invoke(browser, "conversation_draft", { conversationId: conversation.id }), unsent);
    await click(browser, 'button[aria-label="Settings"]');
    await click(browser, "button=Execution policies");
    const liveQuestionTimeout = browser.$('input[id="live-wait"]');
    await liveQuestionTimeout.waitForEnabled();
    assert.equal(await liveQuestionTimeout.getValue(), "10");
    await liveQuestionTimeout.setValue("3");
    await click(browser, '[data-setting="live-wait"] button[type="submit"]');
    await browser.waitUntil(async () => (await invoke(browser, "project_question_timeout", { projectId: main.projectId })) === 3);
    const projectSubagentLimit = browser.$('input[id="subagent-cap"]');
    await projectSubagentLimit.waitForEnabled();
    await projectSubagentLimit.setValue("2");
    await click(browser, "button=Save subagent policy");
    await browser.waitUntil(async () => (await invoke(browser, "project_subagent_policy", { projectId: main.projectId })).maximum === 2);
    const projectAudit = browser.$('select[id="subagent-audit"]');
    await projectAudit.waitForEnabled();
    await projectAudit.selectByAttribute("value", "complete");
    await browser.waitUntil(async () => (await invoke(browser, "project_subagent_audit", { projectId: main.projectId })) === "complete");
    await projectAudit.waitForEnabled();
    await projectAudit.selectByAttribute("value", "available");
    await browser.waitUntil(async () => (await invoke(browser, "project_subagent_audit", { projectId: main.projectId })) === "available");
    await browser.keys("Escape");
    await click(browser, 'button[aria-label="Subagent limits"]');
    const subagentDialog = browser.$('.conv-dialog[role="dialog"]');
    const requested = subagentDialog.$('input[type="number"]');
    await requested.waitForEnabled();
    await replaceControlValue(browser, requested, "5");
    await click(browser, "button=Save limit");
    await browser.waitUntil(async () => (await invoke(browser, "conversation_subagent_quota", { conversationId: conversation.id })).requested === 5);
    const authorized = subagentDialog.$('.//dt[normalize-space()="Authorized now"]/following-sibling::dd[1]');
    assert.equal(await authorized.getText(), "2");
    await capture(browser, "real-subagent-limits");
    await click(subagentDialog, 'summary*=Activity and CLI limits');
    assert.ok((await subagentDialog.getText()).includes("No native subagent activity has been reported."));
    const limitMode = subagentDialog.$('.//label[contains(., "Limit mode")]/select');
    await limitMode.waitForEnabled();
    await limitMode.selectByAttribute("value", "strict");
    const nativeAllowance = subagentDialog.$('.//label[contains(., "Requested native allowance per CLI session")]/input');
    await nativeAllowance.waitForEnabled();
    await replaceControlValue(browser, nativeAllowance, "1");
    await click(browser, "button=Save limit");
    await click(subagentDialog, 'summary*=Audit policy');
    const auditPolicy = subagentDialog.$('.//label[contains(., "Subagent audit policy")]/select');
    await auditPolicy.waitForEnabled();
    await auditPolicy.selectByAttribute("value", "complete");
    await browser.waitUntil(async () => (await invoke(browser, "conversation_subagent_audit", {
      conversationId: conversation.id,
    })).conversationOverride === "complete");
    await browser.waitUntil(async () => {
      const quota = await invoke(browser, "conversation_subagent_quota", { conversationId: conversation.id });
      return quota.modeOverride === "strict" && quota.nativeRequested === 1;
    });
    await limitMode.waitForEnabled();
    await limitMode.selectByAttribute("value", "");
    await click(browser, "button=Save limit");
    const closeSubagentDialog = subagentDialog.$("button=Close");
    await closeSubagentDialog.waitForEnabled();
    await closeSubagentDialog.click();
    const otherConversation = await invoke(browser, "create_conversation", { checkoutId: main.checkoutId, title: "Other conversation" });

    killTestApplication(application);
    await browser.deleteSession().catch(() => undefined);
    browser = await startSession(application);
    assert.equal((await invoke(browser, "active_checkout"))?.checkoutId, main.checkoutId);
    try {
      await browser.$('button[aria-label="Conversations"]').waitForEnabled({ timeout: 15_000 });
    } catch (error) {
      const screen = await browser.execute(() => document.body.innerText.slice(0, 500));
      throw new Error(`Workspace did not reopen: ${screen}`, { cause: error });
    }
    await chooseConversations(browser);
    await clickConversation(browser, otherConversation.title);
    const otherDraft = browser.$(`textarea[id="draft-${otherConversation.id}"]`);
    await otherDraft.waitForEnabled();
    assert.equal(await otherDraft.getValue(), "");
    await chooseConversations(browser);
    await clickConversation(browser, conversation.title);
    const recoveredDraft = browser.$(`textarea[id="draft-${conversation.id}"]`);
    await recoveredDraft.waitForEnabled();
    assert.equal(await recoveredDraft.getValue(), unsent);
    assert.deepEqual(await invoke(browser, "conversation_messages", { conversationId: conversation.id }), []);
    assert.deepEqual(await invoke(browser, "conversation_queue_history_policy", { conversationId: conversation.id }), {
      projectDefault: "recorded",
      conversationOverride: "cancel",
    });
    const persistedQuota = await invoke(browser, "conversation_subagent_quota", { conversationId: conversation.id });
    assert.equal(await invoke(browser, "project_question_timeout", { projectId: main.projectId }), 3);
    assert.equal(persistedQuota.requested, 5);
    assert.equal(persistedQuota.authorized, 2);
    assert.equal(persistedQuota.modeOverride, null);
    assert.equal(persistedQuota.nativeRequested, 1);
    assert.deepEqual(await invoke(browser, "conversation_subagent_audit", { conversationId: conversation.id }), {
      projectDefault: "available", conversationOverride: "complete",
    });
    await invoke(browser, "set_conversation_subagent_audit", { conversationId: conversation.id, policy: null });
    assert.deepEqual(persistedQuota.managed, { active: 0, queued: 0, historical: 0 });
    const mode = browser.$(`section[aria-label="${conversation.title}"] select[aria-label="Conversation mode"]`);
    await mode.selectByAttribute("value", "goal");
    assert.equal(await browser.$("button=Start goal").isEnabled(), false);
    await recoveredDraft.click();
    await browser.keys(["Control", "Enter"]);
    assert.equal(await invoke(browser, "latest_conversation_job", { conversationId: conversation.id }), null);
    await mode.selectByAttribute("value", "read_only");
    await click(browser.$(`section[aria-label=${JSON.stringify(conversation.title)}]`), "button=Goal");
    const goalSettings = browser.$('.conv-goal-dialog[role="dialog"]');
    await goalSettings.waitForDisplayed();
    assert.equal(await browser.execute(() => document.activeElement?.tagName), "TEXTAREA");
    await capture(browser, "real-goal-mode");
    const initialSize = await browser.execute(() => ({ width: innerWidth, height: innerHeight }));
    await invoke(browser, "plugin:window|set_size", { label: "main", value: { Logical: { width: 640, height: 440 } } });
    await browser.waitUntil(() => browser.execute(() => Math.abs(innerWidth - 640) <= 4));
    assert.equal(await browser.execute(() => {
      const dialog = document.querySelector(".conv-goal-dialog");
      const actions = dialog.querySelector("footer").getBoundingClientRect();
      const body = dialog.querySelector(".conv-dialog-body");
      return actions.bottom < innerHeight && actions.top > 0 && body.scrollHeight > body.clientHeight;
    }), true, "Goal actions must remain visible while the form scrolls in a short window");
    await capture(browser, "real-goal-mode-640x440");
    await click(goalSettings, "button=Close");
    await invoke(browser, "plugin:window|set_size", { label: "main", value: { Logical: initialSize } });
    assert.equal(await recoveredDraft.getValue(), unsent);
    if (process.env.OMNIAGENT_LIVE_EFFORTS === "1") {
      const argon = "gemini-4-argon";
      const hydrogen = "gemini-4-hydrogen";
      const find = (models, id) => models.find(({ option }) => option.selection.modelId === id);
      const argonProfile = await invoke(browser, "conversation_models", { provider: "antigravity", modelId: argon });
      const hydrogenProfile = await invoke(browser, "conversation_models", { provider: "antigravity", modelId: hydrogen });
      assert.deepEqual(find(argonProfile, argon)?.efforts, ["low", "medium", "high"]);
      assert.deepEqual(find(hydrogenProfile, hydrogen)?.efforts, ["low", "medium", "high", "xhigh"]);
      assert.deepEqual(find(argonProfile, hydrogen)?.efforts, []);
    }

    if (process.env.OMNIAGENT_LIVE_CONVERSATION === "1") {
      const selector = `section[aria-label="${conversation.title}"]`;
      const panel = browser.$(selector);
      const models = await invoke(browser, "conversation_models", { provider: "codex" });
      const model = models.find(({ option }) => option.selection.modelId === "gpt-6-luna" && option.selectable);
      assert.ok(model, "the installed profile does not offer the qualified fixture model");
      const modelTrigger = `${selector} .model-picker-trigger`;
      await browser.$(modelTrigger).waitForDisplayed();
      await browser.execute((trigger) => document.querySelector(trigger).focus(), modelTrigger);
      await browser.keys("Enter");
      const radioSelector = `.model-picker-menu [role="menuitemradio"][aria-label="${model.option.name}"]`;
      const modelRadio = browser.$(radioSelector);
      await modelRadio.waitForDisplayed();
      await browser.execute((radio) => document.querySelector(radio).focus(), radioSelector);
      const rating = model.option.rating;
      assert.ok(rating);
      await browser.waitUntil(() => browser.$(".model-picker-rating").getAttribute("aria-label").then((label) => label === `Ratings for ${model.option.name}`));
      const originalRating = await browser.execute(() => {
        const option = document.querySelector(".model-picker-rating");
        return {
          meters: [...option.querySelectorAll("meter")].map((meter) => [meter.getAttribute("aria-label"), meter.value]),
          price: [...option.querySelectorAll("span")].find((span) => span.childElementCount === 0
            && span.textContent.startsWith("API reference "))?.textContent,
        };
      });
      assert.deepEqual(originalRating.meters, [
        ["Intelligence", rating.intelligenceTenths / 10],
        ["Price score", rating.priceScore],
      ]);
      const rate = new Intl.NumberFormat("en-US", { style: "currency", currency: "USD" }).format(rating.apiReferenceCents / 100);
      assert.equal(originalRating.price, `API reference ${rate} / 1M tokens`);
      await browser.keys("Enter");
      await browser.$(".model-picker-menu").waitForExist({ reverse: true });
      assert.equal(await browser.$(modelTrigger).getText(), model.option.name);
      const prompt = "Reply exactly OMNIAGENT_DESKTOP_REPLY without tools.";
      await recoveredDraft.setValue(prompt);
      await panel.$("button=Send").click();
      await panel.$("p=OMNIAGENT_DESKTOP_REPLY").waitForDisplayed({ timeout: 45_000 });
      await browser.waitUntil(async () => (await invoke(browser, "latest_conversation_run", {
        conversationId: conversation.id,
      }))?.status === "succeeded");
      assert.equal(await invoke(browser, "conversation_draft", { conversationId: conversation.id }), "");
      const messages = await invoke(browser, "conversation_messages", { conversationId: conversation.id });
      assert.deepEqual(messages.map(({ text }) => text), [prompt, "OMNIAGENT_DESKTOP_REPLY"]);
      await panel.$("button=Activity").click();
      const activity = browser.$('.work-inspector[data-kind="activity"]');
      await click(activity, "button=Exact input");
      const providerInput = activity.$('section[aria-label="Exact provider input"] pre');
      await providerInput.waitForDisplayed();
      const wire = JSON.parse(await providerInput.getText());
      assert.equal(wire.params.input[0].text, prompt);
      await click(activity, 'button[aria-label="Close activity"]');

      await panel.$('select:has(option[value="plan"])').selectByAttribute("value", "plan");
      const researchRows = Array.from({ length: 40 }, (_, index) => `Research row ${index + 1}`).join("\n");
      await recoveredDraft.setValue(`In planning mode, first stream a short public progress message. Then use native request_user_input to ask which file format to use, offering Markdown or JSON. Wait for my answer. Do not edit any files. Here are the fixture rows:\n${researchRows}`);
      await panel.$("button=Send").click();
      await panel.$('div[aria-label="Streaming response"]').waitForDisplayed({ timeout: 45_000 });
      const { id: activeRun } = await invoke(browser, "latest_conversation_run", { conversationId: conversation.id });
      const conversationJob = await invoke(browser, "latest_conversation_job", { conversationId: conversation.id });
      assert.equal(conversationJob.job.latestAttempt.runId, activeRun);
      const queue = await invoke(browser, "project_job_queue", { projectId: checkout.projectId });
      assert.ok(queue.active >= 1);
      assert.equal(queue.jobs.find(({ job }) => job.id === conversationJob.job.id).job.latestAttempt.runId, activeRun);
      const historySelector = `${selector} [aria-label="Conversation history"]`;
      await browser.waitUntil(() => browser.execute((historySelector) => {
        const history = document.querySelector(historySelector);
        return history.scrollHeight - history.clientHeight > 80;
      }, historySelector));
      await browser.execute((historySelector) => { document.querySelector(historySelector).scrollTop = 60; }, historySelector);

      await chooseConversations(browser);
      await clickConversation(browser, otherConversation.title);
      const otherText = "Rascunho na segunda conversa";
      await otherDraft.setValue(otherText);
      await browser.$('section[aria-label="Other conversation"]').$("p=Draft saved").waitForDisplayed();
      const whileSwitched = await invoke(browser, "latest_conversation_run", { conversationId: conversation.id });
      assert.ok(["running", "waiting_for_input"].includes(whileSwitched.status));
      assert.equal(await invoke(browser, "latest_conversation_run", { conversationId: otherConversation.id }), null);

      await chooseConversations(browser);
      await clickConversation(browser, conversation.title);
      await panel.$('div[aria-label="Streaming response"]').waitForDisplayed();
      await panel.$("button=Reply").waitForDisplayed({ timeout: 15_000 });
      assert.equal((await readTabs(browser)).active, conversation.title);
      assert.equal((await readTabs(browser)).groups.flat().filter((title) => title === conversation.title).length, 1);
      assert.equal(await browser.execute((historySelector) => document.querySelector(historySelector).scrollTop, historySelector), 60);

      await browser.$(".dv-active-group .dv-active-tab .dv-default-tab-action").click();
      await panel.waitForExist({ reverse: true });
      await chooseConversations(browser);
      await clickConversation(browser, conversation.title);
      await panel.$('div[aria-label="Streaming response"]').waitForDisplayed();
      assert.equal((await invoke(browser, "latest_conversation_run", { conversationId: conversation.id })).id, activeRun);
      assert.ok((await invoke(browser, "conversation_messages", { conversationId: conversation.id }))
        .some(({ text }) => text === "OMNIAGENT_DESKTOP_REPLY"));
      assert.equal(await invoke(browser, "conversation_draft", { conversationId: otherConversation.id }), otherText);
      await panel.$("button=Stop").click();
      await browser.waitUntil(async () => {
        const run = await invoke(browser, "latest_conversation_run", { conversationId: conversation.id });
        return ["cancelled", "interrupted"].includes(run?.status);
      }, { timeout: 15_000 });
      const userMessages = (await invoke(browser, "conversation_messages", { conversationId: conversation.id }))
        .filter(({ role }) => role === "user").map(({ revisionId }) => revisionId);
      const retryButton = panel.$("button=Retry saved input");
      await retryButton.waitForDisplayed();
      await retryButton.click();
      let retry;
      await browser.waitUntil(async () => {
        const inspection = await invoke(browser, "latest_conversation_job", { conversationId: conversation.id });
        retry = inspection.job.latestAttempt;
        return retry?.previousRunId === activeRun && retry?.recovery === "retry";
      });
      assert.equal(retry.ordinal, 2);
      assert.deepEqual((await invoke(browser, "conversation_messages", { conversationId: conversation.id }))
        .filter(({ role }) => role === "user").map(({ revisionId }) => revisionId), userMessages);
      await invoke(browser, "stop_conversation_turn", { conversationId: conversation.id, runId: retry.runId });
      await browser.waitUntil(async () => {
        const run = await invoke(browser, "latest_conversation_run", { conversationId: conversation.id });
        return run?.id === retry.runId && ["cancelled", "interrupted"].includes(run.status);
      }, { timeout: 15_000 });
    }
    await checkSimulation(browser, main);
    await invoke(browser, "plugin:window|set_size", { label: "main", value: { Logical: { width: 1120, height: 760 } } });
    await browser.waitUntil(() => browser.execute(() => Math.abs(window.innerWidth - 1120) <= 4));
    await t.test("checks the current worktree and required evidence on a linked retry", async () => {
      await verifyJobPolicy(browser, application, scratch);
    });
    await t.test("dispatches queued work while the WebView is detached and restores the same attempt", async () => {
      await verifyDetachedJob(browser, application, scratch);
    });
    await t.test("answers live and expired questions after reopening the desktop", async () => {
      await verifyQuestionContinuation(browser, application, scratch);
    });
    await t.test("edits and removes queued instructions before the active turn ends", async () => {
      await verifyPendingInstructions(browser, application, scratch);
    });
  } catch (error) {
    if (browser && process.env.OMNIAGENT_UI_CAPTURE_DIR) {
      console.error(error.message);
      await capture(browser, "failure").catch(() => undefined);
      const screen = await browser.execute(() => document.body.innerText.slice(0, 1300)).catch(() => "");
      if (screen) console.error(screen);
    }
    throw error;
  } finally {
    try {
      await browser?.deleteSession();
    } finally {
      driver.kill();
    }
  }
});

test("recovers unsent chat and dirty source after a Windows desktop crash and retains deleted history links", { timeout: 120_000 }, async (t) => {
  const { application, scratch } = await copyTestApplication(t);
  const repo = join(scratch, "Recovery checkout");
  const path = "RecoverySource.cs";
  const original = 'class RecoverySource { string Value = "draft"; } // buffer';
  const edited = `${original}x`;
  git(scratch, "init", "-q", repo);
  await writeFile(join(repo, path), original);
  git(repo, "add", path);
  git(repo, "-c", "user.name=OmniAgent Fixture", "-c", "user.email=fixture@example.invalid",
    "-c", "commit.gpgsign=false", "commit", "-q", "-m", "Source fixture");

  const driver = startTestDriver(application);
  let browser;
  let previousCheckout;
  let profile;
  try {
    browser = await connectTestDriver(driver, application);
    previousCheckout = await invoke(browser, "active_checkout");
    const checkout = await invoke(browser, "register_project", { directory: repo });
    await selectCheckout(browser, checkout);
    profile = (await invoke(browser, "cli_executables")).find(({ provider }) => provider === "codex");
    assert.ok(profile);
    await invoke(browser, "set_cli_executable", { provider: "codex", executable: application });
    const fixture = (await invoke(browser, "conversation_models", { provider: "codex" }))
      .find(({ option }) => option.selectable && option.selection.modelId === "desktop-fixture");
    assert.ok(fixture);
    const title = "Desktop recovery acceptance";
    const conversation = await invoke(browser, "create_conversation", { checkoutId: checkout.checkoutId, title });
    const scope = { conversationId: conversation.id };
    const prompt = "Complete desktop slot probe";
    await invoke(browser, "save_conversation_draft", { ...scope, text: prompt });
    await invoke(browser, "start_conversation_turn", { ...scope, text: prompt, mode: "read_only", selection: fixture.option.selection });
    const run = await waitForFixtureResult(browser, checkout.projectId, conversation.id, "The recovery conversation fixture", 10_000);
    const recorded = (await invoke(browser, "conversation_history", scope)).messages.find(({ role }) => role === "user");
    assert.equal(recorded?.text, prompt);
    const attempt = { ...scope, runId: run.id };
    const inventory = await invoke(browser, "conversation_dispatch_inventory", attempt);
    assert.ok(inventory?.dispatchId);
    assert.ok((await invoke(browser, "conversation_input", attempt)).frames.some((frame) => frame.includes(prompt)));

    const openChat = async () => {
      await chooseConversations(browser);
      await clickConversation(browser, title);
      const panel = browser.$(`section[aria-label="${title}"]`);
      const draft = panel.$(`textarea[id="draft-${conversation.id}"]`);
      await draft.waitForEnabled();
      return { panel, draft };
    };
    const unsent = "Keep the next chat message through a Windows crash.";
    const firstChat = await openChat();
    await firstChat.draft.setValue(unsent);
    await firstChat.panel.$("p=Draft saved").waitForDisplayed();

    const openSource = async () => {
      await click(browser, 'button[aria-label="IDE"]');
      const ide = browser.$('section[aria-label="IDE workspace"]');
      const text = ide.$(".code-editor .view-lines");
      if (!(await text.isDisplayed())) await click(ide, `button=${path}`);
      await text.waitForDisplayed();
      return { ide, text };
    };
    const { text } = await openSource();
    await browser.waitUntil(() => browser.execute(() => {
      const tokens = document.querySelectorAll('.code-editor .view-lines span[class^="mtk"]');
      const colors = new Set([...tokens].map((token) => getComputedStyle(token).color));
      return colors.size > 1;
    }), { timeout: 5_000, timeoutMsg: "The C# source should render with syntax highlighting" });
    await text.click();
    await browser.waitUntil(() => browser.execute(() => !!document.activeElement?.closest(".code-editor .monaco-editor")),
      { timeout: 5_000, timeoutMsg: "The source editor did not receive keyboard focus" });
    await browser.action("key").down(Key.End).up(Key.End).perform();
    await browser.keys("x");
    let rendered;
    await browser.waitUntil(async () => {
      rendered = await text.getText();
      return rendered.replaceAll("\u00a0", " ").trimEnd() === edited;
    }, { timeout: 5_000 }).catch((cause) => {
      throw new Error(`The source editor did not render the complete edit (${JSON.stringify(rendered)})`, { cause });
    });
    const source = { checkoutId: checkout.checkoutId, path };
    let sourceDraft;
    await browser.waitUntil(async () => {
      sourceDraft = await invoke(browser, "read_repository_source_draft", source);
      return sourceDraft?.markdown === edited;
    }, { timeout: 10_000, timeoutMsg: "The source editor did not persist its dirty buffer" });
    assert.equal(await readFile(join(repo, path), "utf8"), original);

    killTestApplication(application);
    await browser.deleteSession().catch(() => undefined);
    browser = await startSession(application);
    assert.equal((await invoke(browser, "active_checkout"))?.checkoutId, checkout.checkoutId);
    const recovered = await openSource();
    assert.match(await recovered.text.getText(), /bufferx/);
    assert.match(await recovered.ide.$(".ide-statusbar").getText(), /Unsaved changes/);
    assert.deepEqual(await invoke(browser, "read_repository_source_draft", source), sourceDraft);
    assert.equal(await readFile(join(repo, path), "utf8"), original);

    const chat = await openChat();
    assert.equal(await chat.draft.getValue(), unsent);
    assert.deepEqual((await invoke(browser, "conversation_history", scope)).messages.filter(({ role }) => role === "user"), [recorded]);
    await click(chat.panel, 'button[title="Conversation history controls"]');
    const history = browser.$('.conv-dialog[role="dialog"]');
    await click(history, "summary=Options");
    await click(history, "summary=Delete a revision or diff");
    await history.$('.//label[contains(., "History item")]/select').selectByAttribute("value", `message:${recorded.revisionId}`);
    await history.$('.//label[contains(., "Scope")]/select').selectByAttribute("value", "with_derived_copies");
    await click(history, "button=Review deletion impact");
    const impact = history.$('[aria-label="Deletion impact"]');
    await impact.waitForDisplayed();
    assert.ok((await impact.getText()).includes(inventory.dispatchId));
    await click(history, "button=Confirm deletion");
    await history.$("p=History payload deleted.").waitForDisplayed();
    await browser.keys("Escape");
    await history.waitForExist({ reverse: true });

    assert.deepEqual((await invoke(browser, "conversation_history", scope)).messages.find(({ revisionId }) => revisionId === recorded.revisionId),
      { ...recorded, text: "" });
    assert.equal(await chat.draft.getValue(), unsent);
    assert.equal((await invoke(browser, "conversation_dispatch_inventory", attempt)).dispatchId, inventory.dispatchId);
    const input = await invoke(browser, "conversation_input", attempt);
    assert.equal(input.historyPayloadDeleted, true);
    assert.equal(input.frames.some((frame) => frame.includes(prompt)), false);
    await click(chat.panel, "button=Activity");
    await click(browser, "button=Exact input");
    await browser.$("p=Exact input is unavailable after a history deletion.").waitForDisplayed();
  } finally {
    if (profile) await invoke(browser, "set_cli_executable", { provider: "codex", executable: profile.executable }).catch(() => undefined);
    if (previousCheckout) await invoke(browser, "select_active_checkout", { checkoutId: previousCheckout.checkoutId }).catch(() => undefined);
    try {
      await browser?.deleteSession();
    } finally {
      driver.kill();
    }
  }
});

test("dispatches a local skill, a plugin export and exact same-project conversation revisions from the Windows composer", { timeout: 90_000 }, async (t) => {
  const { application, scratch } = await copyTestApplication(t);
  const repo = join(scratch, "Composer customization checkout");
  const foreignRepo = join(scratch, "Other customization project");
  git(scratch, "init", "-q", repo);
  git(scratch, "init", "-q", foreignRepo);
  const plugin = join(repo, "fixture-installed-plugins", "desktop-formatting");
  const namespace = "desktop-formatting@fixture-marketplace";
  const skills = [
    { path: join(repo, ".agents", "skills", "check-format", "SKILL.md"), name: "check-format", namespace: null, action: "Check repository formatting." },
    { path: join(plugin, "skills", "check-format", "SKILL.md"), name: "desktop-formatting:check-format", namespace, action: "Check exported formatting." },
  ];
  await mkdir(join(plugin, ".codex-plugin"), { recursive: true });
  await writeFile(join(plugin, ".codex-plugin", "plugin.json"), JSON.stringify({ name: "desktop-formatting", version: "1.0.0" }));
  for (const skill of skills) {
    const source = `---\nname: check-format\ndescription: ${skill.action}\n---\n${skill.action}\n`;
    await mkdir(dirname(skill.path), { recursive: true });
    await writeFile(skill.path, source);
    skill.revision = createHash("sha256").update(source).digest("hex");
  }

  const driver = startTestDriver(application);
  let browser;
  let previousCheckout;
  let configured;
  try {
    browser = await connectTestDriver(driver, application);
    const checkout = await invoke(browser, "register_project", { directory: repo });
    const foreign = await invoke(browser, "register_project", { directory: foreignRepo });
    previousCheckout = await invoke(browser, "active_checkout");
    await selectCheckout(browser, checkout);
    configured = (await invoke(browser, "cli_executables")).find(({ provider }) => provider === "codex");
    assert.ok(configured);
    await invoke(browser, "set_cli_executable", { provider: "codex", executable: application });
    const model = (await invoke(browser, "conversation_models", { provider: "codex" }))
      .find(({ option }) => option.selectable && option.selection.modelId === "desktop-fixture");
    assert.ok(model);
    const source = await invoke(browser, "create_conversation", { checkoutId: checkout.checkoutId, title: "Saved customization source" });
    const chat = await invoke(browser, "create_conversation", { checkoutId: checkout.checkoutId, title: "Customization target" });
    const outside = await invoke(browser, "create_conversation", { checkoutId: foreign.checkoutId, title: "Foreign customization source" });
    const settled = (conversationId) => waitForFixtureResult(browser, checkout.projectId, conversationId,
      "The customization fixture", 18_000);
    const prompt = "Complete desktop slot probe";
    await invoke(browser, "save_conversation_draft", { conversationId: source.id, text: prompt });
    await invoke(browser, "start_conversation_turn", { conversationId: source.id, selection: model.option.selection, mode: "read_only", text: prompt });
    await settled(source.id);
    const history = await invoke(browser, "conversation_history", { conversationId: source.id });
    assert.equal(history.messages.length, 2);
    const selection = { kind: "messages", contextId: history.contextId,
      spans: history.messages.map(({ revisionId }) => ({ revisionId })) };
    const foreignScope = { destinationConversationId: outside.id, sourceConversationId: source.id };
    await assert.rejects(invoke(browser, "conversation_reference_source", foreignScope), /same project/i);
    await assert.rejects(invoke(browser, "preview_conversation_reference", { ...foreignScope, selection }), /same project/i);

    await chooseConversations(browser);
    await clickConversation(browser, chat.title);
    const panel = browser.$(`section[aria-label="${chat.title}"]`);
    await click(panel, ".model-picker-trigger");
    await click(browser, `.model-picker-menu [role="menuitemradio"][aria-label="${model.option.name}"]`);
    const draft = panel.$(`textarea[id="draft-${chat.id}"]`);
    await draft.waitForEnabled();
    const text = "Apply the selected local skill and plugin export.";
    await draft.setValue(text);
    await browser.keys("End");
    const skillLabel = ({ namespace, name }) => namespace ? `${namespace} · ${name}` : name;
    for (const skill of skills) {
      await click(panel, "button=Skills & plugin exports");
      const actions = browser.$('[aria-label="Composer actions"]');
      await actions.waitForDisplayed();
      await capture(browser, "real-composer-action-autocomplete");
      await click(actions, `.//*[@role="option"][.//strong[normalize-space(.)="${skillLabel(skill)}"]]`);
    }
    const selectedSkills = panel.$('[aria-label="Selected skills and plugin exports"]');
    for (const skill of skills) {
      await selectedSkills.$(`strong=${skillLabel(skill)}`).waitForDisplayed();
      await selectedSkills.$(`code[title="${skill.revision}"]`).waitForDisplayed();
    }

    await click(panel, 'button[aria-label="Find conversation references"]');
    const references = browser.$('[aria-label="Conversation references"]');
    const query = references.$('[aria-label="Search conversation references"]');
    await query.setValue(outside.title);
    await references.$("p=No matching conversations.").waitForDisplayed();
    assert.equal(await references.$(`button[aria-label="Open source ${outside.title}"]`).isExisting(), false);
    await query.setValue(source.title);
    await click(references, `button[aria-label="Choose excerpt or summary from ${source.title}"]`);
    const checkboxes = () => references.$$('input[type="checkbox"]');
    await browser.waitUntil(async () => (await checkboxes()).length === history.messages.length);
    const excerpts = await checkboxes();
    for (const message of excerpts) await message.click();
    await click(references, "button=Preview selection");
    const preview = references.$('[aria-label="Exact conversation reference"]');
    await preview.waitForDisplayed();
    for (const message of history.messages) {
      assert.ok((await preview.getText()).includes(message.revisionId));
      assert.ok((await preview.getText()).includes(message.text));
    }
    await click(references, "button=Attach reference");
    await panel.$('[aria-label="Attached conversation references"]').waitForDisplayed();
    const [attached] = await invoke(browser, "conversation_reference_draft", { conversationId: chat.id });
    assert.deepEqual(attached.artifact.excerpts.map(({ source, text }) => [source.origin.revisionId, text]),
      history.messages.map(({ revisionId, text }) => [revisionId, text]));
    await assert.rejects(invoke(browser, "edit_conversation_reference_draft", {
      conversationId: outside.id, previewId: attached.id, attach: true,
    }), /unavailable for this destination/i);

    await click(panel, "button=Send");
    const run = await settled(chat.id);
    const attempt = { conversationId: chat.id, runId: run.id };
    assert.equal(run.checkoutId, checkout.checkoutId);
    const { profile } = await invoke(browser, "conversation_profile_evidence", { ...attempt, projectId: checkout.projectId });
    assert.equal(profile.provider, "codex");
    assert.equal(profile.profileId, model.option.selection.profileId);
    assert.equal(profile.requestedModel, model.option.selection.modelId);
    await panel.$("p=Received both native skill inputs and the selected chat excerpts.").waitForDisplayed();
    const inventory = await invoke(browser, "conversation_dispatch_inventory", attempt);
    const saved = inventory.inventory.requestedSkill.selections;
    const normalized = (path) => path.replaceAll("\\", "/");
    assert.deepEqual(saved.map(({ name, path, namespace, revision, provider }) => ({ name, path: normalized(path), namespace, revision, provider })),
      skills.map(({ name, path, namespace, revision }) => ({ name, path: normalized(path), namespace, revision, provider: "codex" })));
    const input = await invoke(browser, "conversation_input", attempt);
    const committed = (await invoke(browser, "conversation_history", { conversationId: chat.id })).messages.find(({ role }) => role === "user");
    assert.deepEqual(input.conversationReferences, [{ messageRevisionId: committed.revisionId, reference: attached }]);
    const wire = JSON.parse(input.frames[0]);
    assert.deepEqual(wire.params.input.filter(({ type }) => type === "skill"), saved.map(({ name, path }) => ({ type: "skill", name, path })));
    const rendered = wire.params.input.find(({ type }) => type === "text").text;
    assert.ok(rendered.startsWith(text));
    assert.ok(!rendered.includes("@skill:"));
    assert.deepEqual(JSON.parse(rendered.split("Quoted conversation references supplied by OmniAgent (JSON data):\n")[1]), input.conversationReferences);
  } finally {
    if (configured) await invoke(browser, "set_cli_executable", { provider: "codex", executable: configured.executable }).catch(() => undefined);
    if (previousCheckout) await invoke(browser, "select_active_checkout", { checkoutId: previousCheckout.checkoutId }).catch(() => undefined);
    try {
      await browser?.deleteSession();
    } finally {
      driver.kill();
    }
  }
});

test("builds, simulates and executes a real pipeline visually", { timeout: 180_000 }, async (t) => {
  const { application, scratch } = await copyTestApplication(t);
  const repo = join(scratch, "Visual pipeline checkout");
  git(scratch, "init", "-q", repo);
  await writeFile(join(repo, "README.md"), "# Visual pipeline test\n");
  git(repo, "add", "README.md");
  git(repo, "-c", "user.name=OmniAgent Fixture", "-c", "user.email=fixture@example.invalid",
    "-c", "commit.gpgsign=false", "commit", "-qm", "Visual pipeline fixture");

  const driver = startTestDriver(application);
  let browser;
  try {
    browser = await connectTestDriver(driver, application);
    const checkout = await invoke(browser, "register_project", { directory: repo });
    await selectCheckout(browser, checkout);
    await click(browser, 'button[aria-label="Pipelines"]');
    await click(browser.$('nav[aria-label="Pipelines views"]'), "button=Repository delivery");
    await browser.$('.repo-delivery [aria-label="Repository Markdown source"]').$("button=Select Markdown file").waitForDisplayed();
    await capture(browser, "real-repository-delivery");
    await verifyVisualPipelineBuilder({
      browser, application, checkout, invoke, capture, emulateDisplay,
      restart: async (current) => {
        killTestApplication(application);
        await current.deleteSession().catch(() => undefined);
        browser = await startSession(application);
        return browser;
      },
    });
  } finally {
    await browser?.deleteSession().catch(() => undefined);
    driver.kill();
  }
});

test("publishes and launches the Audit factory from prompt and Markdown", { timeout: 180_000 }, async (t) => {
  const { application, scratch } = await copyTestApplication(t);
  const repo = join(scratch, "Audit factory checkout");
  git(scratch, "init", "-q", repo);
  await writeFile(join(repo, "README.md"), "# Audit factory test\n");
  git(repo, "add", "README.md");
  git(repo, "-c", "user.name=OmniAgent Fixture", "-c", "user.email=fixture@example.invalid",
    "-c", "commit.gpgsign=false", "commit", "-qm", "Audit factory fixture");

  const driver = startTestDriver(application);
  let browser;
  try {
    browser = await connectTestDriver(driver, application);
    const checkout = await invoke(browser, "register_project", { directory: repo });
    await selectCheckout(browser, checkout);
    await verifyAuditFactory({ browser, application, checkout, invoke, capture });
  } finally {
    await browser?.deleteSession().catch(() => undefined);
    driver.kill();
  }
});

test("compares committed branches and registers a manual worktree in the real Git workspace", { timeout: 90_000 }, async (t) => {
  const { application, scratch } = await copyTestApplication(t);
  const repo = join(scratch, "Git workspace repository");
  const worktree = join(scratch, "Manual worktree");
  git(scratch, "init", "-q", "-b", "main", repo);
  git(repo, "config", "user.name", "OmniAgent Fixture");
  git(repo, "config", "user.email", "fixture@example.invalid");
  git(repo, "config", "commit.gpgsign", "false");
  git(repo, "config", "core.autocrlf", "false");
  await writeFile(join(repo, "README.md"), "# Main branch\n");
  git(repo, "add", "README.md");
  git(repo, "commit", "-qm", "Main revision");
  git(repo, "switch", "-qc", "topic");
  await writeFile(join(repo, "README.md"), "# Topic branch\n");
  git(repo, "commit", "-qam", "Topic revision");
  git(repo, "switch", "-q", "main");
  await writeFile(join(repo, "README.md"), "# Unsaved working tree edit\n");

  const driver = startTestDriver(application);
  let browser;
  try {
    browser = await connectTestDriver(driver, application);
    const checkout = await invoke(browser, "register_project", { directory: repo });
    await selectCheckout(browser, checkout);
    await click(browser, '.work-rail button[aria-label="Git"]');
    const gitView = browser.$('[aria-label="Git workspace"]');
    const source = gitView.$('select[aria-label="Diff source"]');
    await source.waitForEnabled();
    await source.selectByAttribute("value", "branch");
    const base = gitView.$('select[aria-label="Base reference"]');
    await base.waitForEnabled();
    await base.selectByAttribute("value", "refs/heads/main");
    const compare = gitView.$('select[aria-label="Compare reference"]');
    await compare.waitForEnabled();
    await compare.selectByAttribute("value", "refs/heads/topic");
    const diff = gitView.$('[aria-label="Diff editor for README.md"]');
    await diff.waitForDisplayed({ timeout: 20_000 });
    await browser.waitUntil(async () => {
      const rendered = await diff.getProperty("textContent");
      return /Main\s+branch/.test(rendered) && /Topic\s+branch/.test(rendered) && !rendered.includes("Unsaved working tree edit");
    }, { timeout: 10_000, timeoutMsg: "The branch comparison should show committed versions, not dirty checkout content" });
    await capture(browser, "real-git-branch-comparison");

    await click(gitView, "button=Worktrees");
    await click(gitView, "button=New worktree");
    const dialog = browser.$('[role="dialog"][aria-labelledby]');
    await dialog.waitForDisplayed();
    await dialog.$('.//label[normalize-space(.)="Branch"]/input').setValue("manual-worktree");
    await dialog.$('.//label[starts-with(normalize-space(.), "Worktree path")]/input').setValue(worktree);
    await click(dialog, "button=Create worktree");
    await dialog.waitForExist({ reverse: true });
    assert.equal(git(worktree, "branch", "--show-current").trim(), "manual-worktree");
    assert.equal(await readFile(join(worktree, "README.md"), "utf8"), "# Main branch\n");
    assert.equal(await readFile(join(repo, "README.md"), "utf8"), "# Unsaved working tree edit\n");
    const registered = await invoke(browser, "registered_checkouts");
    const created = registered.find(({ checkoutRoot }) => checkoutRoot.toLowerCase() === worktree.toLowerCase());
    assert.equal(created?.projectId, checkout.projectId);
    await capture(browser, "real-git-manual-worktree");
  } finally {
    await browser?.deleteSession().catch(() => undefined);
    driver.kill();
  }
});

test("keeps a line review comment bound to its saved Git snapshot", { timeout: 90_000 }, async (t) => {
  const { application, scratch } = await copyTestApplication(t);
  const repo = join(scratch, "Inline review checkout");
  const path = "sample.rs";
  const commentText = "Check this exact review revision";
  git(scratch, "init", "-q", "-b", "main", repo);
  git(repo, "config", "core.autocrlf", "false");
  await writeFile(join(repo, path), "fn original() {}\n");
  git(repo, "add", path);
  git(repo, "-c", "user.name=OmniAgent Fixture", "-c", "user.email=fixture@example.invalid",
    "-c", "commit.gpgsign=false", "commit", "-qm", "Review base");
  await writeFile(join(repo, path), "fn revised() {}\n");

  const driver = startTestDriver(application);
  let browser;
  try {
    browser = await connectTestDriver(driver, application);
    const checkout = await invoke(browser, "register_project", { directory: repo });
    await selectCheckout(browser, checkout);
    await click(browser, '.work-rail button[aria-label="Git"]');
    const gitView = browser.$('[aria-label="Git workspace"]');
    await click(gitView, "button=Review hunks");
    const editedLine = gitView.$('.//*[@aria-label="Git hunks"]//span[normalize-space(.)="+fn revised() {}"]');
    await editedLine.waitForDisplayed();
    await editedLine.click({ button: "right" });
    await click(browser, '//*[@role="menuitem"][normalize-space(.)="Add review comment on revised line…"]');
    const dialog = browser.$('[role="dialog"].file-dialog');
    await dialog.waitForDisplayed();
    await dialog.$("textarea").setValue(commentText);
    await click(dialog, "button=Save comment");
    const saved = gitView.$('[data-review-comment]');
    await saved.waitForDisplayed();
    assert.match(await saved.getText(), /sample\.rs:1 · Revised/);
    assert.match(await saved.getText(), /Current comparison/);
    assert.ok((await saved.getText()).includes(commentText));

    await writeFile(join(repo, path), "fn latest() {}\n");
    await browser.refresh();
    await click(browser, '.work-rail button[aria-label="Git"]');
    const reloaded = browser.$('[aria-label="Git workspace"]');
    await click(reloaded, "summary*=Review comments");
    const persisted = reloaded.$('[data-review-comment]');
    await persisted.waitForDisplayed();
    assert.match(await persisted.getText(), /Saved snapshot/);
    assert.ok((await persisted.getText()).includes(commentText));
    await click(persisted, "summary=Reviewed text and revision");
    assert.equal(await persisted.$("pre").getText(), "fn revised() {}");
  } finally {
    await browser?.deleteSession().catch(() => undefined);
    driver.kill();
  }
});

test("discards only the reviewed Git hunk and preserves the staged file", { timeout: 90_000 }, async (t) => {
  const { application, scratch } = await copyTestApplication(t);
  const repo = join(scratch, "Discard reviewed hunk checkout");
  const path = "reviewed-hunk.txt";
  const context = Array.from({ length: 18 }, (_, index) => `unchanged ${index}\n`).join("");
  const text = (start, end) => `${start}\n${context}${end}\n`;
  git(scratch, "init", "-q", "-b", "main", repo);
  git(repo, "config", "core.autocrlf", "false");
  await writeFile(join(repo, path), text("committed", "original tail"));
  git(repo, "add", path);
  git(repo, "-c", "user.name=OmniAgent Fixture", "-c", "user.email=fixture@example.invalid",
    "-c", "commit.gpgsign=false", "commit", "-qm", "Commit before edits");
  await writeFile(join(repo, path), text("staged", "original tail"));
  git(repo, "add", path);
  await writeFile(join(repo, path), text("only this edit goes away", "keep this working edit"));
  const index = await readFile(join(repo, ".git", "index"));

  const driver = startTestDriver(application);
  let browser;
  try {
    browser = await connectTestDriver(driver, application);
    const checkout = await invoke(browser, "register_project", { directory: repo });
    await selectCheckout(browser, checkout);
    await click(browser, '.work-rail button[aria-label="Git"]');
    const gitView = browser.$('[aria-label="Git workspace"]');
    await click(gitView, "button=Review hunks");
    const firstHunkActions = '[aria-label="Git hunks"] section[aria-label^="@@ -1,"] > .diff-hunk-menu > .context-actions-more';
    const openFirstHunkActions = async () => {
      await browser.waitUntil(() => browser.execute((selector) => {
        const trigger = document.querySelector(selector);
        if (!(trigger instanceof HTMLButtonElement)) return false;
        trigger.focus();
        return document.activeElement === trigger;
      }, firstHunkActions), { timeoutMsg: "The first Git hunk did not render its action button." });
      await browser.keys(Key.Enter);
    };
    await openFirstHunkActions();
    await click(browser, '//*[@role="menuitem"][normalize-space(.)="Mark hunk as reviewed"]');
    const reviewed = '//*[@aria-label="Git hunks"]//span[normalize-space(.)="Reviewed"]';
    await browser.$(reviewed).waitForDisplayed();
    assert.deepEqual(await readFile(join(repo, ".git", "index")), index);
    assert.equal(await readFile(join(repo, path), "utf8"), text("only this edit goes away", "keep this working edit"));
    await browser.refresh();
    await click(browser, '.work-rail button[aria-label="Git"]');
    await click(gitView, "button=Review hunks");
    await browser.$(reviewed).waitForDisplayed();
    await openFirstHunkActions();
    await click(browser, '//*[@role="menuitem"][normalize-space(.)="Discard hunk…"]');
    const preview = browser.$('[role="dialog"].file-comparison-dialog');
    await preview.$('[aria-label="Discard preview"]').waitForDisplayed();
    assert.equal(await readFile(join(repo, path), "utf8"), text("only this edit goes away", "keep this working edit"));
    await click(preview, "button=Discard reviewed changes");
    await preview.waitForExist({ reverse: true });
    assert.equal(await readFile(join(repo, path), "utf8"), text("staged", "keep this working edit"));
    assert.deepEqual(await readFile(join(repo, ".git", "index")), index);
  } finally {
    await browser?.deleteSession().catch(() => undefined);
    driver.kill();
  }
});

test("opens Git and local file history and recycles only the clicked checkout file", { timeout: 90_000 }, async (t) => {
  const { application, scratch } = await copyTestApplication(t);
  const repo = join(scratch, "Explorer context checkout");
  const retained = "retained.txt";
  const original = "Original retained text\n";
  const updated = "Original retained textx\n";
  const removed = `OmniAgent-context-smoke-${randomUUID()}.txt`;
  git(scratch, "init", "-q", "-b", "main", repo);
  git(repo, "config", "core.autocrlf", "false");
  await writeFile(join(repo, retained), original);
  git(repo, "add", retained);
  git(repo, "-c", "user.name=OmniAgent Fixture", "-c", "user.email=fixture@example.invalid",
    "-c", "commit.gpgsign=false", "commit", "-qm", "Retained file revision");
  await writeFile(join(repo, removed), "Disposable file for the Explorer context menu\n");

  const driver = startTestDriver(application);
  let browser;
  try {
    browser = await connectTestDriver(driver, application);
    const checkout = await invoke(browser, "register_project", { directory: repo });
    await selectCheckout(browser, checkout);
    await click(browser, '.work-rail button[aria-label="IDE"]');
    const ide = browser.$('[aria-label="IDE workspace"]');
    const retainedRow = ide.$(`button[title="${retained}"]`);
    await retainedRow.waitForDisplayed();
    await browser.execute((name) => document.querySelector(`[aria-label="IDE workspace"] button[title="${CSS.escape(name)}"]`).focus(), retained);
    await browser.keys([Key.Shift, Key.F10]);
    await click(browser, '//*[@role="menuitem"][normalize-space(.)="Show file history"]');
    const history = browser.$('[aria-label="File history"]');
    await history.$("button*=Retained file revision").waitForDisplayed();

    await click(browser, '.work-rail button[aria-label="IDE"]');
    await click(ide, `button=${retained}`);
    const editor = ide.$(".code-editor .view-lines");
    await editor.waitForDisplayed();
    await editor.$(".view-line:first-child").click();
    await browser.action("key").down(Key.End).up(Key.End).perform();
    await browser.keys("x");
    const source = { checkoutId: checkout.checkoutId, path: retained };
    await browser.waitUntil(async () => (await invoke(browser, "read_repository_source_draft", source))?.markdown === updated);
    await click(ide, 'button[aria-label="Save file"]');
    await browser.waitUntil(async () => (await readFile(join(repo, retained), "utf8")) === updated);

    await retainedRow.click({ button: "right" });
    await click(browser, '//*[@role="menuitem"][normalize-space(.)="Show local history"]');
    const localHistory = browser.$('[role="dialog"].file-comparison-dialog');
    await click(localHistory, "button*=Before saving");
    await localHistory.$('select[aria-label="Revision comparison"]').selectByAttribute("value", "text");
    const preview = localHistory.$('[aria-label="Local revision preview"] pre');
    await preview.waitForDisplayed();
    assert.equal(await preview.getText(), original.trimEnd());
    await click(localHistory, "button=Load text into editor");
    await localHistory.waitForExist({ reverse: true });
    assert.equal((await invoke(browser, "read_repository_source_draft", source))?.markdown, original);
    assert.equal(await readFile(join(repo, retained), "utf8"), updated);

    const root = ide.$(".file-tree-root > button.file-tree-row");
    await root.waitForDisplayed();
    await root.click({ button: "right" });
    const templates = browser.$('//*[@role="menuitem"][normalize-space(.)="New from template"]');
    await templates.waitForDisplayed();
    await templates.moveTo();
    await click(browser, '//*[@role="menuitem"][normalize-space(.)="Markdown note"]');
    const create = browser.$('[role="dialog"].file-dialog');
    await create.waitForDisplayed();
    await create.$('input[aria-label="Name"]').setValue("new-from-template");
    await click(create, "button=Create");
    await create.waitForExist({ reverse: true });
    assert.equal(await readFile(join(repo, "new-from-template.md"), "utf8"), "# Title\n\n");

    await browser.$('.work-rail button[aria-label="Knowledge"][aria-current="page"]').waitForDisplayed();
    await click(browser, '.work-rail button[aria-label="IDE"]');
    const disposableRow = ide.$(`button[title="${removed}"]`);
    await disposableRow.waitForDisplayed();
    await disposableRow.click({ button: "right" });
    await click(browser, '//*[@role="menuitem"][normalize-space(.)="Move to Recycle Bin…"]');
    const confirmation = browser.$('[role="dialog"].file-dialog');
    await confirmation.waitForDisplayed();
    assert.ok((await confirmation.getText()).includes(removed));
    await click(confirmation, "button=Move to Recycle Bin");
    await confirmation.waitForExist({ reverse: true, timeout: 15_000 });
    await disposableRow.waitForExist({ reverse: true });
    await assert.rejects(access(join(repo, removed)), { code: "ENOENT" });
    assert.equal(await readFile(join(repo, retained), "utf8"), updated);
  } finally {
    await browser?.deleteSession().catch(() => undefined);
    driver.kill();
  }
});

test("converts IDE encoding and line endings only after reviewing the saved bytes", { timeout: 90_000 }, async (t) => {
  const { application, scratch } = await copyTestApplication(t);
  const repo = join(scratch, "Source format checkout");
  const file = "encoding.txt";
  const source = join(repo, file);
  const original = Buffer.from("Café\r\nSecond\n");
  git(scratch, "init", "-q", "-b", "main", repo);
  git(repo, "config", "core.autocrlf", "false");
  await writeFile(source, original);
  git(repo, "add", ".");
  git(repo, "-c", "user.name=OmniAgent Fixture", "-c", "user.email=fixture@example.invalid",
    "-c", "commit.gpgsign=false", "commit", "-qm", "Seed mixed line endings");
  const utf16 = (text) => Buffer.concat([Buffer.from([0xff, 0xfe]), Buffer.from(text, "utf16le")]);

  const driver = startTestDriver(application);
  let browser;
  try {
    browser = await connectTestDriver(driver, application);
    const checkout = await invoke(browser, "register_project", { directory: repo });
    await selectCheckout(browser, checkout);
    await click(browser, '.work-rail button[aria-label="IDE"]');
    const ide = browser.$('[aria-label="IDE workspace"]');
    await click(ide, `button=${file}`);
    const focusMenuItem = async (label) => {
      const item = browser.$(`//*[@role="menuitem"][normalize-space(.)="${label}"]`);
      await item.waitForDisplayed();
      assert.equal(await browser.execute((text) => {
        const target = [...document.querySelectorAll('[role="menuitem"]')]
          .find((entry) => entry.textContent?.trim() === text);
        if (!(target instanceof HTMLElement)) return false;
        target.focus();
        return document.activeElement === target;
      }, label), true);
    };
    const review = async (saved, action, option) => {
      const format = ide.$(`[aria-label="Saved file format ${saved}"]`);
      await format.waitForDisplayed();
      await format.click({ button: "right" });
      await focusMenuItem(action);
      await browser.keys(Key.ArrowRight);
      await focusMenuItem(option);
      await browser.keys(Key.Enter);
      const dialog = browser.$('[role="dialog"].file-dialog');
      await dialog.waitForDisplayed();
      await dialog.$('[aria-label="Converted initial bytes"]').waitForDisplayed();
      return dialog;
    };

    const preview = await review("UTF-8, Mixed line endings", "Change encoding", "UTF-16 LE with BOM");
    assert.match(await preview.$('[aria-label="Converted initial bytes"]').getText(), /^FF FE/);
    await click(preview, "button=Cancel");
    await preview.waitForExist({ reverse: true });
    assert.deepEqual(await readFile(source), original);

    const encoding = await review("UTF-8, Mixed line endings", "Change encoding", "UTF-16 LE with BOM");
    await click(encoding, "button=Convert and save");
    await encoding.waitForExist({ reverse: true });
    assert.deepEqual(await readFile(source), utf16("Café\r\nSecond\n"));

    const endings = await review("UTF-16 LE with BOM, Mixed line endings", "Convert line endings", "CRLF");
    await click(endings, "button=Convert and save");
    await endings.waitForExist({ reverse: true });
    assert.deepEqual(await readFile(source), utf16("Café\r\nSecond\r\n"));

    const line = ide.$(".code-editor .view-line:first-child");
    await line.waitForDisplayed();
    await line.click();
    await browser.action("key").down(Key.End).up(Key.End).perform();
    await browser.keys("!");
    await click(ide, 'button[aria-label="Save file"]');
    await browser.waitUntil(async () => (await readFile(source)).equals(utf16("Café!\r\nSecond\r\n")));
    const history = await invoke(browser, "checkout_document_history", { checkoutId: checkout.checkoutId, path: file, before: null });
    const newest = await invoke(browser, "checkout_document_history_text", { checkoutId: checkout.checkoutId, path: file, position: history.entries[0].position });
    assert.equal(newest.encoding, "utf16_le");
    assert.equal(newest.lineEndings, "crlf");
    assert.equal(newest.text, "Café!\nSecond\n");
  } finally {
    await browser?.deleteSession().catch(() => undefined);
    driver.kill();
  }
});

test("detaches the selected dirty editor into a native window and returns it with its draft", { timeout: 90_000 }, async (t) => {
  const { application, scratch } = await copyTestApplication(t);
  const repo = join(scratch, "Detached editor checkout");
  const file = "draft.txt";
  const other = "other.txt";
  const note = "meeting.md";
  const original = "Draft content\n";
  const revised = "Draft contentx\n";
  const editedInWindow = "Draft contentxy\n";
  git(scratch, "init", "-q", "-b", "main", repo);
  git(repo, "config", "core.autocrlf", "false");
  await writeFile(join(repo, file), original);
  await writeFile(join(repo, other), "Other file\n");
  await writeFile(join(repo, note), "# Meeting notes\n");
  git(repo, "add", ".");
  git(repo, "-c", "user.name=OmniAgent Fixture", "-c", "user.email=fixture@example.invalid",
    "-c", "commit.gpgsign=false", "commit", "-qm", "Seed detached editor checkout");

  const driver = startTestDriver(application);
  let browser;
  try {
    browser = await connectTestDriver(driver, application);
    const main = await browser.getWindowHandle();
    const checkout = await invoke(browser, "register_project", { directory: repo });
    const source = { checkoutId: checkout.checkoutId, path: file };
    await selectCheckout(browser, checkout);
    await click(browser, '.work-rail button[aria-label="IDE"]');
    const ide = browser.$('[aria-label="IDE workspace"]');
    await click(ide, `button=${file}`);
    const editor = ide.$(".code-editor .view-line:first-child");
    await editor.waitForDisplayed();
    await editor.click();
    await browser.action("key").down(Key.End).up(Key.End).perform();
    await browser.keys("x");
    await browser.waitUntil(async () => (await invoke(browser, "read_repository_source_draft", source))?.markdown === revised);
    await click(ide, `button=${other}`);
    await ide.$(`.file-tab-label[title="${file}"]`).click({ button: "right" });
    await click(browser, '//*[@role="menuitem"][normalize-space(.)="Move tab to new window"]');
    await browser.waitUntil(async () => (await browser.getWindowHandles()).some((handle) => handle !== main));
    const labels = () => invoke(browser, "plugin:window|get_all_windows");
    const waitForMainOnly = () => browser.waitUntil(async () =>
      (await browser.getWindowHandles()).length === 1 && (await labels()).every((label) => !label.startsWith("editor-")),
    { timeoutMsg: "The detached editor's native or browser window stayed open." });
    const switchToDetached = async (path) => {
      await browser.waitUntil(async () => (await browser.getWindowHandles()).length === 2);
      const handle = (await browser.getWindowHandles()).find((candidate) => candidate !== main);
      await browser.switchToWindow(handle);
      await browser.waitUntil(() => browser.execute((file) => {
        return [...document.querySelectorAll(".file-tab-label")].some((tab) => tab.getAttribute("title") === file);
      }, path).catch(() => false), { timeoutMsg: `The detached editor did not load the tab for ${path}.` });
    };
    assert.ok((await labels()).some((label) => label.startsWith("editor-")));
    await switchToDetached(file);
    const popupTab = browser.$(`.file-tab-label[title="${file}"]`);
    await popupTab.waitForDisplayed();
    const detachedEditor = browser.$(".code-editor .view-line:first-child");
    await detachedEditor.waitForDisplayed();
    assert.match(await detachedEditor.getText(), /Draft contentx/);
    assert.equal(await browser.$$(".file-tab-label").length, 1);
    await detachedEditor.click();
    await browser.action("key").down(Key.End).up(Key.End).perform();
    await browser.keys("y");
    await browser.waitUntil(async () => (await detachedEditor.getText()).includes("Draft contentxy"));
    await click(popupTab, `button[aria-label="Close ${file}"]`);
    const dirtyClose = browser.$('[role="dialog"].file-dialog');
    await dirtyClose.waitForDisplayed();
    await click(dirtyClose, "button=Cancel");
    await popupTab.click({ button: "right" });
    await click(browser, '//*[@role="menuitem"][normalize-space(.)="Return editor window to main window"]');
    await browser.switchToWindow(main);
    await waitForMainOnly();
    await ide.$(`.file-tab-label[title="${file}"]`).waitForDisplayed();
    await browser.waitUntil(async () => (await invoke(browser, "read_repository_source_draft", source))?.markdown === editedInWindow);
    assert.equal(await readFile(join(repo, file), "utf8"), original);

    await ide.$(".file-dock-group-menu").click({ button: "right" });
    await click(browser, '//*[@role="menuitem"][normalize-space(.)="Move group to new window"]');
    await switchToDetached(file);
    await browser.$(`.file-tab-label[title="${file}"]`).waitForDisplayed();
    await browser.$(`.file-tab-label[title="${other}"]`).waitForDisplayed();
    await browser.$(".file-dock-group-menu").click({ button: "right" });
    await click(browser, '//*[@role="menuitem"][normalize-space(.)="Return editor window to main window"]');
    await browser.switchToWindow(main);
    await waitForMainOnly();

    await ide.$(`button[title="${note}"]`).click({ button: "right" });
    await click(browser, '//*[@role="menuitem"][normalize-space(.)="Open in new window"]');
    await switchToDetached(note);
    const noteTab = browser.$(`.file-tab-label[title="${note}"]`);
    await noteTab.waitForDisplayed();
    const noteEditor = browser.$(`[aria-label="Markdown editor for ${note}"] .cm-line:first-child`);
    await noteEditor.waitForDisplayed();
    await noteEditor.click();
    await browser.action("key").down(Key.End).up(Key.End).perform();
    await browser.keys("!");
    await browser.waitUntil(async () => (await noteEditor.getText()).includes("Meeting notes!"));
    await noteTab.click({ button: "right" });
    await click(browser, '//*[@role="menuitem"][normalize-space(.)="Return editor window to main window"]');
    await browser.switchToWindow(main);
    await waitForMainOnly();
    await browser.waitUntil(async () => (await invoke(browser, "read_repository_markdown_draft", {
      checkoutId: checkout.checkoutId, path: note,
    }))?.markdown === "# Meeting notes!\n");
  } finally {
    await browser?.deleteSession().catch(() => undefined);
    driver.kill();
  }
});

test("filters actual Windows hidden attributes independently from dotfiles in Explorer", { timeout: 90_000 }, async (t) => {
  const { application, scratch } = await copyTestApplication(t);
  const repo = join(scratch, "Windows hidden items checkout");
  git(scratch, "init", "-q", repo);
  const hiddenFile = "hidden-note.md";
  const hiddenFolder = "reports/private";
  const ordinary = "reports/ordinary.md";
  const dotfile = ".visible-dotfile";
  await mkdir(join(repo, hiddenFolder), { recursive: true });
  await writeFile(join(repo, hiddenFile), "A Windows Hidden file\n");
  await writeFile(join(repo, hiddenFolder, "saved.md"), "An ordinary file under a Windows Hidden folder\n");
  await writeFile(join(repo, ordinary), "A visible file\n");
  await writeFile(join(repo, dotfile), "A dotfile without the Windows Hidden attribute\n");
  for (const relative of [hiddenFile, hiddenFolder]) {
    const markHidden = spawnSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command",
      "$fixturePath = $env:OMNIAGENT_HIDDEN_ITEM_FIXTURE\n"
      + "$attributes = [IO.File]::GetAttributes($fixturePath)\n"
      + "[IO.File]::SetAttributes($fixturePath, ($attributes -bor [IO.FileAttributes]::Hidden))"], {
      env: { ...process.env, OMNIAGENT_HIDDEN_ITEM_FIXTURE: join(repo, relative) }, encoding: "utf8", windowsHide: true,
    });
    if (markHidden.error) throw markHidden.error;
    assert.equal(markHidden.status, 0, markHidden.stderr);
  }

  const driver = startTestDriver(application);
  let browser;
  try {
    browser = await connectTestDriver(driver, application);
    const checkout = await invoke(browser, "register_project", { directory: repo });
    assert.deepEqual(await invoke(browser, "list_checkout_windows_hidden_items", { checkoutId: checkout.checkoutId, folder: "" }), [hiddenFile]);
    assert.deepEqual(await invoke(browser, "list_checkout_windows_hidden_items", { checkoutId: checkout.checkoutId, folder: "reports" }), [hiddenFolder]);
    await selectCheckout(browser, checkout);
    await click(browser, '.work-rail button[aria-label="IDE"]');
    const ide = browser.$('[aria-label="IDE workspace"]');
    const row = (path) => ide.$(`[data-tree-item][title="${path}"]`);
    await row(hiddenFile).waitForDisplayed();
    await click(ide, '[data-tree-item][title="reports"]');
    await row(hiddenFolder).waitForDisplayed();
    const checkoutRoot = ide.$(".file-tree-root > button.file-tree-row");

    await checkoutRoot.click({ button: "right" });
    await click(browser, '//*[@role="menuitem"][normalize-space(.)="Hide Windows hidden items"]');
    await row(ordinary).waitForDisplayed();
    assert.equal(await row(hiddenFile).isExisting(), false);
    assert.equal(await row(hiddenFolder).isExisting(), false);
    await row(dotfile).waitForDisplayed();

    await checkoutRoot.click({ button: "right" });
    await click(browser, '//*[@role="menuitem"][normalize-space(.)="Show Windows hidden items"]');
    await row(hiddenFile).waitForDisplayed();
    await row(hiddenFolder).waitForDisplayed();
    await row(dotfile).waitForDisplayed();
  } finally {
    await browser?.deleteSession().catch(() => undefined);
    driver.kill();
  }
});

test("opens OmniAgent file links in a running desktop and on cold launch", { timeout: 90_000 }, async (t) => {
  const { application, scratch } = await copyTestApplication(t);
  const repo = join(scratch, "File link repository");
  const linked = join(scratch, "Exact linked worktree");
  const file = "Revisão #2.ts";
  const original = "first\nsecond\nthird\n";
  git(scratch, "init", "-q", repo);
  await writeFile(join(repo, file), "Wrong worktree\n");
  git(repo, "add", ".");
  git(repo, "-c", "user.name=OmniAgent Fixture", "-c", "user.email=fixture@example.invalid",
    "-c", "commit.gpgsign=false", "commit", "-qm", "Base file link checkout");
  git(repo, "worktree", "add", "-q", "--detach", linked);
  git(linked, "config", "core.autocrlf", "false");
  await writeFile(join(linked, file), original);

  const driver = startTestDriver(application);
  let browser;
  let cold;
  try {
    browser = await connectTestDriver(driver, application);
    const primary = await invoke(browser, "register_project", { directory: repo });
    const target = await invoke(browser, "register_project", { directory: linked });
    await selectCheckout(browser, primary);
    const query = new URLSearchParams({ checkout: target.checkoutId, path: file, line: "2", column: "3" });
    const link = `omniagent://open-file/?${query}`;

    await showTestApplication(browser, application, [link]);
    const ide = browser.$('[aria-label="IDE workspace"]');
    const editor = ide.$(".code-editor .view-lines");
    await editor.waitForDisplayed();
    assert.match(await editor.getText(), /second/);
    assert.equal((await invoke(browser, "active_checkout"))?.checkoutId, primary.checkoutId);
    await browser.waitUntil(() => browser.execute(() => {
      const input = document.querySelector('.code-editor .monaco-editor [role="textbox"]');
      if (!(input instanceof HTMLElement)) return false;
      input.focus();
      return input === document.activeElement;
    }));
    await browser.keys("X");
    let draft;
    await browser.waitUntil(async () => {
      draft = await invoke(browser, "read_repository_source_draft", { checkoutId: target.checkoutId, path: file });
      return draft?.markdown === "first\nseXcond\nthird\n";
    }).catch(async (cause) => {
      throw new Error(`The linked cursor did not apply the test edit (${JSON.stringify({
        draft: draft?.markdown, editor: await editor.getText(),
      })})`, { cause });
    });
    assert.equal(await readFile(join(linked, file), "utf8"), original);

    killTestApplication(application);
    await browser.deleteSession().catch(() => undefined);
    browser = undefined;
    // EdgeDriver treats launch arguments as switches. Start the fixture with a bare URI
    // like the installed protocol handler does, then attach to its isolated WebView2.
    const debuggerAddress = `127.0.0.1:${port + 2}`;
    cold = spawn(application, [link], { stdio: "ignore", windowsHide: true,
      env: { ...fixtureEnvironment(application), WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port + 2}` } });
    await new Promise((resolve, reject) => { cold.once("spawn", resolve); cold.once("error", reject); });
    let debuggerReady = false;
    for (let attempt = 0; attempt < 60; attempt++) {
      assert.equal(cold.exitCode, null, "Cold link fixture exited before we could attach");
      try {
        const response = await fetch(`http://${debuggerAddress}/json/list`, { signal: AbortSignal.timeout(500) });
        debuggerReady = (await response.json()).some(({ url }) => new URL(url).origin === packagedOrigin);
        if (debuggerReady) break;
      } catch {
        // The disposable WebView2 process can take time to expose its debug port.
      }
      await delay(200);
    }
    assert.ok(debuggerReady, "Cold link fixture never exposed its WebView2 debug port");
    browser = await remote({ hostname: "127.0.0.1", port: port + 1, path: "/", logLevel: "warn",
      connectionRetryCount: 0, connectionRetryTimeout: 15_000,
      capabilities: { browserName: "webview2", "wdio:enforceWebDriverClassic": true,
        "ms:edgeChromium": true, "ms:edgeOptions": { debuggerAddress } },
    });
    await waitForRuntime(browser, { startup: true });
    assert.equal(await invoke(browser, "plugin:app|identifier"), testIdentifier(application));
    const restored = browser.$('[aria-label="IDE workspace"] .code-editor .view-lines');
    await restored.waitForDisplayed();
    assert.match(await restored.getText(), /seXcond/);
    assert.equal((await invoke(browser, "active_checkout"))?.checkoutId, primary.checkoutId);
    assert.equal(await readFile(join(linked, file), "utf8"), original);
  } finally {
    await browser?.deleteSession().catch(() => undefined);
    if (cold?.exitCode === null && cold.signalCode === null && cold.pid) {
      const closed = new Promise((resolve) => cold.once("close", resolve));
      cold.kill();
      await closed;
    }
    driver.kill();
  }
});

test("persists real conversation organization through a desktop restart", { timeout: 90_000 }, async (t) => {
  const { application, scratch } = await copyTestApplication(t);
  const repo = join(scratch, "Conversation library repository");
  const otherRepo = join(scratch, "Conversation library second repository");
  const otherTitle = "A conversation in a different repository";
  const title = "A conversation to organize";
  git(scratch, "init", "-q", repo);
  git(scratch, "init", "-q", otherRepo);
  git(repo, "-c", "user.name=OmniAgent Fixture", "-c", "user.email=fixture@example.invalid",
    "-c", "commit.gpgsign=false", "commit", "-q", "--allow-empty", "-m", "Library fixture");

  const driver = startTestDriver(application);
  let browser;
  const library = () => browser.$('section[aria-label="Conversation library"]');
  const row = (scope = library()) => scope.$(`button[aria-label=${JSON.stringify(title)}]`);
  async function menuAction(label) {
    await row().waitForDisplayed();
    await row().click({ button: "right" });
    await click(browser.$('.conv-menu[role="menu"]'), `span=${label}`);
  }

  try {
    browser = await connectTestDriver(driver, application);
    const checkout = await invoke(browser, "register_project", { directory: repo });
    const otherCheckout = await invoke(browser, "register_project", { directory: otherRepo });
    await invoke(browser, "create_conversation", { checkoutId: checkout.checkoutId, title });
    await invoke(browser, "create_conversation", { checkoutId: otherCheckout.checkoutId, title: otherTitle });
    await selectCheckout(browser, checkout);
    await chooseConversations(browser);
    await row().waitForDisplayed();
    assert.equal(await library().$('select[aria-label="Project"]').isExisting(), false);
    assert.equal(await library().$(`button[aria-label=${JSON.stringify(otherTitle)}]`).isExisting(), false);
    const scopedTitles = (await invoke(browser, "conversation_library", { query: null, projectId: checkout.projectId }))
      .map(({ conversation }) => conversation.title);
    assert.deepEqual(scopedTitles, [title]);
    await row().click({ button: "right" });
    await click(browser.$('.conv-menu[role="menu"]'), "span=Section");
    await click(browser, "span=New section…");
    const sectionEditor = browser.$('.conv-dialog[role="dialog"]');
    await sectionEditor.waitForDisplayed();
    await sectionEditor.$("input").setValue("Release notes");
    await click(sectionEditor, "button=Save");
    await row(library().$('section[aria-label="Release notes"]')).waitForDisplayed();
    await menuAction("Mark as unread");
    await browser.waitUntil(async () => await row().getAttribute("aria-description") === "Unread");
    await menuAction("Pin");
    await row(library().$('section[aria-label="Pinned"]')).waitForDisplayed();
    await capture(browser, "real-conversations-pinned");
    await menuAction("Archive");
    await row().waitForExist({ reverse: true });

    killTestApplication(application);
    await browser.deleteSession().catch(() => undefined);
    browser = await startSession(application);
    await chooseConversations(browser);
    assert.equal(await row().isExisting(), false);
    await click(library(), 'button[aria-label="Archived"]');
    await row().waitForDisplayed();
    assert.equal(await row().getAttribute("aria-description"), "Unread");
    await capture(browser, "real-conversations-archived");
    await menuAction("Unarchive");
    await row().waitForExist({ reverse: true });
    await click(library(), 'button[aria-label="Archived"]');
    await row(library().$('section[aria-label="Pinned"]')).waitForDisplayed();
    await menuAction("Unpin");
    await row(library().$('section[aria-label="Release notes"]')).waitForDisplayed();
    await clickConversation(browser, title);
    await browser.waitUntil(async () => (await readTabs(browser)).active === title);
    await browser.waitUntil(async () => await row().getAttribute("aria-description") !== "Unread");
  } finally {
    await browser?.deleteSession().catch(() => undefined);
    driver.kill();
  }
});

test("starts real planning from the saved Markdown revision and finds it from Knowledge", { timeout: 90_000 }, async (t) => {
  const { application, scratch } = await copyTestApplication(t);
  const repo = join(scratch, "Saved notes repository");
  const path = "Release plan.md";
  git(scratch, "init", "-q", repo);
  await writeFile(join(repo, path), "# Release plan\n\nReview the saved rollout and verify its recovery step.\n");
  git(repo, "add", path);
  git(repo, "-c", "user.name=OmniAgent Fixture", "-c", "user.email=fixture@example.invalid",
    "-c", "commit.gpgsign=false", "commit", "-qm", "Save planning note");

  const driver = startTestDriver(application);
  let browser;
  try {
    browser = await connectTestDriver(driver, application);
    const { projectId, checkoutId } = await invoke(browser, "register_project", { directory: repo });
    await selectCheckout(browser, { projectId, checkoutId });
    const opened = await invoke(browser, "open_repository_document", { checkoutId, path });
    assert.equal(opened.status, "document");
    const revisionId = opened.document.revision.id;
    const noteScope = { projectId, checkoutId, revisionId };
    assert.deepEqual(await invoke(browser, "repository_note_planning", noteScope), []);

    await click(browser, '.work-rail button[aria-label="Knowledge"]');
    const knowledge = browser.$('[aria-label="Knowledge workspace"]');
    await click(knowledge, `button=${path}`);
    await click(knowledge, "button=Plan from note");
    const dialog = browser.$('[role="dialog"][aria-labelledby]');
    await dialog.waitForDisplayed();
    assert.ok((await dialog.getText()).includes(path));
    await click(dialog, "button=Start planning");
    await browser.$('[aria-label="Pipelines workspace"]').waitForDisplayed();
    let planned;
    await browser.waitUntil(async () => {
      planned = await invoke(browser, "repository_note_planning", noteScope);
      return planned.length === 1;
    }, { timeout: 15_000, timeoutMsg: "Knowledge did not find planning for the exact saved revision" });
    const delivery = await invoke(browser, "inspect_repository_delivery", { projectId, runId: planned[0].runId });
    assert.equal(delivery.run.id, planned[0].runId);
    assert.equal(delivery.launch?.source?.id, revisionId);
    assert.equal(planned[0].task, null);
    await browser.$("button=Queue planning").waitForDisplayed();
    await browser.waitUntil(() => browser.execute(() => {
      const graph = document.querySelector('[aria-label="Delivery Git graph"] .react-flow');
      if (!graph) return false;
      const style = getComputedStyle(graph);
      const card = style.getPropertyValue("--card").trim();
      return Boolean(card) && style.getPropertyValue("--xy-node-background-color").trim() === card;
    }), { timeoutMsg: "Delivery Git nodes should use the application's card theme" });
    await capture(browser, "real-plan-from-note");
    await click(browser, 'button[aria-label="Back to Knowledge"]');
    await knowledge.$("button=Plan from note").waitForDisplayed();
    assert.equal(await browser.$('.work-rail button[aria-label="Knowledge"]').getAttribute("aria-current"), "page");
  } finally {
    await browser?.deleteSession().catch(() => undefined);
    driver.kill();
  }
});

test("creates and edits real project work from the shared Tasks editor", { timeout: 90_000 }, async (t) => {
  const { application, scratch } = await copyTestApplication(t);
  const repo = join(scratch, "Project work repository");
  git(scratch, "init", "-q", repo);
  git(repo, "-c", "user.name=OmniAgent Fixture", "-c", "user.email=fixture@example.invalid",
    "-c", "commit.gpgsign=false", "commit", "-q", "--allow-empty", "-m", "Project work fixture");

  const driver = startTestDriver(application);
  let browser;
  try {
    browser = await connectTestDriver(driver, application);
    const checkout = await invoke(browser, "register_project", { directory: repo });
    await selectCheckout(browser, checkout);
    const hierarchy = () => invoke(browser, "project_work_hierarchy", { projectId: checkout.projectId });
    await click(browser, '.work-rail button[aria-label="Tasks"]');
    const tasks = browser.$('[aria-label="Tasks workspace"]');
    const editor = browser.$('.agile-dialog[role="dialog"]');
    const field = (label, tag) => editor.$(`.//label[starts-with(normalize-space(.), '${label}')]/${tag}`);
    const newWork = async (type, title) => {
      await click(tasks, "button=New work");
      await editor.waitForDisplayed();
      await field("Work type", "select").selectByAttribute("value", type);
      await field("Title", "input").setValue(title);
    };

    await newWork("epic", "Ship the workbench");
    await click(editor, "button=Create work");
    await editor.waitForExist({ reverse: true });
    const epic = (await hierarchy()).epics.find(({ title }) => title === "Ship the workbench");
    assert.ok(epic);
    await tasks.$('select[aria-label="Kanban level"]').selectByAttribute("value", "epic");
    await click(tasks, "button=Ship the workbench");
    const detail = tasks.$('section[aria-label="Ship the workbench"]');
    await click(detail, "button=Workflow");
    await click(detail, "button=Work history");

    await newWork("task", "Exercise the release");
    const parent = field("Epic", "select");
    await parent.waitForEnabled();
    await parent.selectByAttribute("value", epic.id);
    await field("Acceptance criteria", "textarea").setValue("Verify the read-only launch.");
    await click(editor, "button=Create work");
    await editor.waitForExist({ reverse: true });
    const created = (await hierarchy()).work.find(({ task }) => task.title === "Exercise the release");
    assert.equal(created?.placement.epicId, epic.id);
    await tasks.$('select[aria-label="Kanban level"]').selectByAttribute("value", "task");
    await click(tasks, "button=Exercise the release");
    await click(tasks, "button=Edit task");
    await field("Priority", "select").waitForEnabled();
    await field("Priority", "select").selectByAttribute("value", "1");
    await capture(browser, "real-task-editor");
    await click(editor, "button=Save work");
    await editor.waitForExist({ reverse: true });
    const saved = await invoke(browser, "workspace_work_editor", { projectId: checkout.projectId, workId: created.task.id });
    assert.equal(saved.draft.settings.priority, 1);
    assert.deepEqual(saved.draft.settings.acceptanceCriteria, ["Verify the read-only launch."]);
  } finally {
    await browser?.deleteSession().catch(() => undefined);
    driver.kill();
  }
});
