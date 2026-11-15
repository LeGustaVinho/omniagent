import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { access, copyFile, mkdir, mkdtemp, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { remote } from "webdriverio";

const identifier = process.env.OMNIAGENT_DESKTOP_IDENTIFIER ?? "io.github.legustavinho.omniagent.desktop-tests";
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
  await browser.waitUntil(() => browser.execute(async () => {
    if (location.origin === "null" || typeof window.__TAURI_INTERNALS__?.invoke !== "function") return false;
    try {
      return await window.__TAURI_INTERNALS__.invoke("runtime_status") === "Desktop runtime is ready.";
    } catch {
      return false;
    }
  }), { timeout: 15_000 });
  return browser;
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

async function click(browser, selector) {
  const control = browser.$(selector);
  await control.waitForDisplayed();
  await control.waitForEnabled();
  await control.click();
}

async function capture(browser, name) {
  const directory = process.env.OMNIAGENT_UI_CAPTURE_DIR;
  if (!directory) return;
  await mkdir(directory, { recursive: true });
  await browser.saveScreenshot(join(directory, name + ".png"));
}

async function checkSimulation(browser, checkout) {
  const conversationScope = { checkoutId: checkout.checkoutId };
  const before = await invoke(browser, "list_conversations", conversationScope);
  const settingsBefore = await invoke(browser, "appearance_settings");
  for (const area of ["IDE", "Tasks", "Pipelines", "Knowledge", "Git"]) {
    await click(browser, 'button[aria-label="' + area + '"]');
    assert.equal(await browser.$('button[aria-label="' + area + '"]').getAttribute("aria-current"), "page");
  }
  await click(browser, 'button[aria-label="Knowledge"]');
  await click(browser, ".work-titlebar button=Simulate");
  const simulation = browser.$('[aria-label="Simulation controls"]');
  await simulation.waitForDisplayed();

  await click(browser, "button=Plan from note");
  await browser.$("legend=Who should the recovery instructions address?").waitForExist();
  await capture(browser, "01-conversations");
  await click(browser, "button=Reply");
  await click(browser, "button=Approve implementation");
  await click(browser, "button=Open release task");
  await click(browser, 'button[aria-label="Queue"]');
  await click(browser, "button=Start attempt");
  await click(browser, "button=Capture candidate");
  await click(browser, 'button[aria-label="Close queue"]');
  await click(browser, "button=Run verification");
  await click(browser, "button=Review saved diff");
  await capture(browser, "02-saved-changes");
  await click(browser, "button=Accept diff");
  await click(browser, 'button[aria-label="Close changes"]');

  const scenario = simulation.$("select");
  await scenario.selectByAttribute("value", "recovery");
  await click(browser, 'button[aria-label="Queue"]');
  await click(browser, "button=Resume from checkpoint");
  await browser.$("button=Start attempt").waitForEnabled();
  await scenario.selectByAttribute("value", "pipelines");
  await browser.$(".react-flow").waitForDisplayed();
  await capture(browser, "03-pipelines");
  await scenario.selectByAttribute("value", "windows");
  await click(browser, "button=Create backup");
  await click(browser, "button=Continue");
  await click(browser, "button=Continue");
  await click(browser, "button=Finish");
  await capture(browser, "04-settings");
  await click(browser, 'button[aria-label="Close settings"]');
  await click(browser, "button=Exit simulation");
  await simulation.waitForExist({ reverse: true });
  assert.equal(await browser.$('button[aria-label="Knowledge"]').getAttribute("aria-current"), "page");
  assert.equal((await invoke(browser, "active_checkout")).checkoutId, checkout.checkoutId);
  assert.deepEqual(await invoke(browser, "list_conversations", conversationScope), before);
  assert.deepEqual(await invoke(browser, "appearance_settings"), settingsBefore);
  for (const [width, height] of [[1440, 900], [1120, 760], [640, 440]]) {
    await invoke(browser, "plugin:window|set_size", { label: "main", value: { Logical: { width, height } } });
    await browser.waitUntil(() => browser.execute((wanted) => Math.abs(window.innerWidth - wanted) <= 4, width));
    assert.equal(await browser.execute(() => document.documentElement.scrollWidth <= window.innerWidth + 1), true);
    await capture(browser, "05-workspace-" + width);
  }
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
    return timeline ? Array.from(timeline.children, (row) => row.lastElementChild.textContent) : [];
  }, runId.slice(0, 8));
}

test("registers a Git checkout and runs a fixture in the Windows desktop", { timeout: 150_000 }, async (t) => {
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
  await writeFile(join(repo, "AGENTS.md"), "Desktop registration keeps Markdown available.");
  await mkdir(join(repo, ".claude"));
  await mkdir(join(repo, "scripts"));
  await writeFile(join(repo, "scripts", "startup.ps1"), "Set-Content hook-marker started");
  await writeFile(join(repo, "scripts", "mcp.js"), "require('node:fs').writeFileSync('mcp-marker', 'started');");
  await writeFile(join(repo, ".claude", "settings.json"), JSON.stringify({
    hooks: { SessionStart: [{ matcher: "startup", hooks: [{ type: "command", command: "powershell.exe -NoProfile -File ./scripts/startup.ps1" }] }] },
  }));
  await writeFile(join(repo, ".mcp.json"), JSON.stringify({ mcpServers: { fixture: { command: "node", args: ["./scripts/mcp.js"] } } }));

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
    const registered = await invoke(browser, "registered_checkouts");
    assert.ok(registered.some((item) => item.checkoutId === main.checkoutId));
    assert.ok(registered.some((item) => item.checkoutId === checkout.checkoutId));
    assert.equal(main.projectId, checkout.projectId);
    assert.notEqual(main.checkoutId, checkout.checkoutId);
    assert.equal(await realpath(checkout.checkoutRoot), await realpath(linked));
    await selectCheckout(browser, checkout);
    await browser.$('button[aria-label="Diagnostics"]').click();
    await browser.$("button=Start fixture").waitForDisplayed();

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
    await browser.$("button=Fixture runs").click();
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
      const saved = await invoke(browser, "workbench_layout", { checkoutId: checkout.checkoutId, workspace: "diagnostics" });
      return saved && JSON.parse(saved).grid.root.data.length === 2;
    });

    await browser.deleteSession();
    browser = null;
    browser = await startSession(application);
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
    const reopen = spawn(application, [], { stdio: "ignore", windowsHide: true });
    const stopped = new Promise((resolve, reject) => {
      reopen.once("error", reject);
      reopen.once("exit", resolve);
    });
    try {
      await browser.waitUntil(visible);
      assert.equal(await stopped, 0);
    } finally {
      if (reopen.exitCode === null) reopen.kill();
    }
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
    await browser.$("button=Providers").click();
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
    const draft = browser.$('textarea[id^="draft-"]');
    await draft.waitForEnabled();
    const unsent = "Rascunho — revisar depois ☕";
    await draft.setValue(unsent);
    await browser.$("p=Draft saved").waitForDisplayed();
    const [conversation] = await invoke(browser, "list_conversations", { checkoutId: main.checkoutId });
    assert.equal(await invoke(browser, "conversation_draft", { conversationId: conversation.id }), unsent);
    assert.deepEqual(await invoke(browser, "conversation_messages", { conversationId: conversation.id }), []);
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
    await browser.$("button=Other conversation").click();
    const otherDraft = browser.$(`textarea[id="draft-${otherConversation.id}"]`);
    await otherDraft.waitForEnabled();
    assert.equal(await otherDraft.getValue(), "");
    await chooseConversations(browser);
    await browser.$(`button=${conversation.title}`).click();
    const recoveredDraft = browser.$(`textarea[id="draft-${conversation.id}"]`);
    await recoveredDraft.waitForEnabled();
    assert.equal(await recoveredDraft.getValue(), unsent);
    assert.deepEqual(await invoke(browser, "conversation_messages", { conversationId: conversation.id }), []);

    if (process.env.OMNIAGENT_LIVE_CONVERSATION === "1") {
      const selector = `section[aria-label="${conversation.title}"]`;
      const panel = browser.$(selector);
      const models = await invoke(browser, "conversation_models", { provider: "codex" });
      const model = models.find(({ option }) => option.selection.modelId === "gpt-6-luna" && option.selectable);
      assert.ok(model, "the installed profile does not offer the qualified fixture model");
      const summary = `${selector} form details summary`;
      await browser.$(summary).waitForDisplayed();
      await browser.execute((summary) => document.querySelector(summary).focus(), summary);
      await browser.keys("Enter");
      const radioSelector = `${selector} input[aria-label="${model.option.name}"]`;
      const modelRadio = browser.$(radioSelector);
      await modelRadio.waitForDisplayed();
      await browser.execute((radio) => document.querySelector(radio).focus(), radioSelector);
      await browser.keys(" ");
      await browser.waitUntil(() => modelRadio.isSelected());
      const rating = model.option.rating;
      assert.ok(rating);
      const originalRating = await browser.execute((radio) => {
        const option = document.querySelector(radio).closest("label");
        return {
          meters: [...option.querySelectorAll("meter")].map((meter) => [meter.getAttribute("aria-label"), meter.value]),
          price: [...option.querySelectorAll("span")].find((span) => span.childElementCount === 0
            && span.textContent.startsWith("API reference "))?.textContent,
        };
      }, radioSelector);
      assert.deepEqual(originalRating.meters, [
        ["Intelligence", rating.intelligenceTenths / 10],
        ["Price score", rating.priceScore],
      ]);
      const rate = new Intl.NumberFormat("en-US", { style: "currency", currency: "USD" }).format(rating.apiReferenceCents / 100);
      assert.equal(originalRating.price, `API reference ${rate} / 1M tokens`);
      const compact = await browser.execute((radio) => {
        const picker = document.querySelector(radio).closest("details");
        const selected = picker.querySelector("summary [aria-label='Selected models']");
        picker.style.maxWidth = "315px";
        try {
          return {
            width: picker.clientWidth,
            scrollWidth: picker.scrollWidth,
            meters: [...selected.querySelectorAll("meter")].map((meter) => [meter.getAttribute("aria-label"), meter.value]),
            price: [...selected.querySelectorAll("span")].find((span) => span.childElementCount === 0
              && span.textContent.startsWith("API reference "))?.textContent,
          };
        } finally {
          picker.style.removeProperty("max-width");
        }
      }, radioSelector);
      assert.ok(compact.width > 0 && compact.width <= 315);
      assert.equal(compact.scrollWidth, compact.width);
      assert.deepEqual(compact.meters, originalRating.meters);
      assert.equal(compact.price, originalRating.price);
      await panel.$('summary span[aria-label="Selected models"]').click();
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
      await panel.$("button=Exact input").click();
      await panel.$('section[aria-label="Exact provider input"] pre').waitForDisplayed();
      const wire = JSON.parse(await panel.$('section[aria-label="Exact provider input"] pre').getText());
      assert.equal(wire.params.input[0].text, prompt);

      await panel.$('select:has(option[value="plan"])').selectByAttribute("value", "plan");
      const researchRows = Array.from({ length: 40 }, (_, index) => `Research row ${index + 1}`).join("\n");
      await recoveredDraft.setValue(`In planning mode, first stream a short public progress message. Then use native request_user_input to ask which file format to use, offering Markdown or JSON. Wait for my answer. Do not edit any files. Here are the fixture rows:\n${researchRows}`);
      await panel.$("button=Send").click();
      await panel.$('div[aria-label="Streaming response"]').waitForDisplayed({ timeout: 45_000 });
      const { id: activeRun } = await invoke(browser, "latest_conversation_run", { conversationId: conversation.id });
      const historySelector = `${selector} [aria-label="Conversation history"]`;
      await browser.waitUntil(() => browser.execute((historySelector) => {
        const history = document.querySelector(historySelector);
        return history.scrollHeight - history.clientHeight > 80;
      }, historySelector));
      await browser.execute((historySelector) => { document.querySelector(historySelector).scrollTop = 60; }, historySelector);

      await chooseConversations(browser);
      await browser.$("button=Other conversation").click();
      const otherText = "Rascunho na segunda conversa";
      await otherDraft.setValue(otherText);
      await browser.$('section[aria-label="Other conversation"]').$("p=Draft saved").waitForDisplayed();
      const whileSwitched = await invoke(browser, "latest_conversation_run", { conversationId: conversation.id });
      assert.ok(["running", "waiting_for_input"].includes(whileSwitched.status));
      assert.equal(await invoke(browser, "latest_conversation_run", { conversationId: otherConversation.id }), null);

      await chooseConversations(browser);
      await browser.$(`button=${conversation.title}`).click();
      await panel.$('div[aria-label="Streaming response"]').waitForDisplayed();
      await panel.$("button=Reply").waitForDisplayed({ timeout: 15_000 });
      assert.equal((await readTabs(browser)).active, conversation.title);
      assert.equal((await readTabs(browser)).groups.flat().filter((title) => title === conversation.title).length, 1);
      assert.equal(await browser.execute((historySelector) => document.querySelector(historySelector).scrollTop, historySelector), 60);

      await browser.$(".dv-active-group .dv-active-tab .dv-default-tab-action").click();
      await panel.waitForExist({ reverse: true });
      await chooseConversations(browser);
      await browser.$(`button=${conversation.title}`).click();
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
    }
    await checkSimulation(browser, main);
  } finally {
    try {
      await browser?.deleteSession();
    } finally {
      driver.kill();
    }
  }
});
