import assert from "node:assert/strict";
import { createHash, randomUUID } from "node:crypto";
import { mkdir, mkdtemp, realpath, writeFile } from "node:fs/promises";
import { basename, dirname, join, relative } from "node:path";
import { DatabaseSync } from "node:sqlite";

const title = "Installed protected inspector acceptance";
const skillList = 'ul[aria-label="Selected native skill inputs"]';
const completed = new Set(["succeeded", "failed", "cancelled", "interrupted"]);
const normalized = (path) => path.replaceAll("\\", "/");
const digest = (text) => createHash("sha256").update(text).digest("hex");

async function click(scope, selector) {
  const target = scope.$(selector);
  await target.waitForDisplayed({ timeout: 20_000 });
  await target.waitForEnabled({ timeout: 20_000 });
  await target.click();
}

async function openConversation(browser) {
  await click(browser, 'button[aria-label="Conversations"]');
  const message = browser.$(`section[aria-label=${JSON.stringify(title)}]`);
  if (!(await message.isDisplayed())) {
    const library = browser.$('section[aria-label="Conversation library"]');
    const toggle = browser.$('button[aria-label="Toggle conversation navigation"]');
    await browser.waitUntil(async () => await library.isDisplayed() || await toggle.isDisplayed(), { timeout: 20_000 });
    if (!(await library.isDisplayed()) && await toggle.getAttribute("aria-expanded") === "false") {
      await toggle.click();
    }
    await click(library, `button[aria-label=${JSON.stringify(title)}]`);
  }
  await message.waitForDisplayed({ timeout: 20_000 });
  return message;
}

async function activeDatabase(browser, invoke, dataRoot) {
  const identifier = await invoke(browser, "plugin:app|identifier");
  assert.match(identifier, /^io\.github\.legustavinho\.omniagent\.(?:nsis-test-[0-9a-f]{10}|desktop-tests\.run-[0-9a-f]{32})$/);
  const root = await realpath(dataRoot);
  assert.equal(basename(root), identifier);
  const { active } = await invoke(browser, "application_restore_status");
  if (active) {
    assert.match(active.generationId, /^[0-9a-f-]{36}$/);
  }
  const path = active ? join(root, "restored-generations", active.generationId, "omniagent.sqlite3") : join(root, "omniagent.sqlite3");
  assert.equal(await realpath(path), path);
  return path;
}

function assertProtectedRows(database, { runId, conversationId, dispatchId }, prompt) {
  const db = new DatabaseSync(database, { readOnly: true });
  try {
    const row = db.prepare(`
      SELECT dispatches.frames, context_snapshots.payload
      FROM dispatches JOIN context_snapshots ON context_snapshots.id = dispatches.snapshot_id
      WHERE context_snapshots.run_id = ? AND dispatches.id = ? AND dispatches.delivery = 'submitted'
    `).get(runId, dispatchId);
    assert.ok(row, "The exact dispatch must belong to the retained attempt");
    for (const bytes of [row.frames, row.payload]) {
      assert.ok(bytes instanceof Uint8Array && bytes.length > 0);
      assert.ok(!Buffer.from(bytes).includes(Buffer.from(prompt)), "Protected dispatch bytes contained the plaintext prompt");
    }
    const { count } = db.prepare("SELECT count(*) AS count FROM agent_attempts WHERE conversation_id = ?").get(conversationId);
    assert.equal(count, 1, "Opening retained evidence must not start another attempt");
  } finally {
    db.close();
  }
}

export async function runInstalledProtectedInspector({ application, checkout, repo, dataRoot, withInstalledWindow, invoke, run }) {
  const configuredFixture = process.env.OMNIAGENT_DESKTOP_FIXTURE_CLI;
  const fixture = configuredFixture ? await realpath(configuredFixture) : null;
  if (fixture) {
    assert.notEqual(fixture, await realpath(application));
    assert.equal(run(fixture, ["login", "status"]), "Logged in to the desktop fixture");
  }
  const nonce = randomUUID().replaceAll("-", "");
  const modelId = fixture ? "desktop-fixture" : "gpt-6-luna";
  const skillName = fixture ? "check-format" : `omniagent-inspect-${nonce.slice(0, 8)}`;
  const prompt = fixture ? "Complete desktop slot probe" : `Apply the selected local skill, reply only with INSPECTOR_${nonce}, and make no file changes.`;
  const root = await realpath(await mkdtemp(join(dirname(repo), "protected-inspector-")));
  const skillPath = join(root, ".agents", "skills", skillName, "SKILL.md");
  const instructionPath = join(root, "AGENTS.md");
  const skillText = `---\nname: ${skillName}\ndescription: Verify the recorded installer skill selection.\n---\nFollow the request without changing files.\n`;
  const instructionText = "# Inspector fixture\nLeave this disposable checkout unchanged.\n";
  await mkdir(dirname(skillPath), { recursive: true });
  await writeFile(skillPath, skillText, "utf8");
  await writeFile(instructionPath, instructionText, "utf8");
  run("git", ["init", "--quiet", "--template="], { cwd: root });
  run("git", ["add", "--", relative(root, skillPath), relative(root, instructionPath)], { cwd: root });
  run("git", ["-c", "core.hooksPath=.git/acceptance-hooks", "-c", "user.name=Installer Fixture",
    "-c", "user.email=fixture@example.invalid", "-c", "commit.gpgsign=false", "commit", "-qm", "Seed inspector fixtures"], { cwd: root });

  const saved = await withInstalledWindow(async (browser) => {
    const database = await activeDatabase(browser, invoke, dataRoot);
    const originalCheckout = await invoke(browser, "active_checkout");
    const originalCli = (await invoke(browser, "cli_executables")).find(({ provider }) => provider === "codex");
    assert.ok(originalCli);
    const registered = await invoke(browser, "register_project", { directory: root });
    assert.notEqual(registered.projectId, checkout.projectId);
    const { settings } = await invoke(browser, "project_job_queue", { projectId: registered.projectId });
    try {
      await invoke(browser, "set_cli_executable", { provider: "codex", executable: fixture });
      if (settings.paused) await invoke(browser, "set_job_queue_paused", { paused: false });
      await invoke(browser, "select_active_checkout", { checkoutId: registered.checkoutId });
      const conversation = await invoke(browser, "create_conversation", { checkoutId: registered.checkoutId, title });
      const foreign = await invoke(browser, "create_conversation", { checkoutId: checkout.checkoutId, title: "Protected inspector scope control" });
      const model = (await invoke(browser, "conversation_models", { provider: "codex" }))
        .find(({ option }) => option.selectable && option.selection.modelId === modelId)?.option;
      assert.ok(model, `The isolated inspector must provide the ${modelId} model`);
      await browser.refresh();
      const panel = await openConversation(browser);
      await click(panel, ".model-picker-trigger");
      await click(browser, `.model-picker-menu [role="menuitemradio"][aria-label=${JSON.stringify(model.name)}]`);
      const mode = panel.$('select[aria-label="Conversation mode"]');
      await mode.waitForEnabled();
      await mode.selectByAttribute("value", "read_only");
      const draft = panel.$(`textarea[id="draft-${conversation.id}"]`);
      await draft.waitForEnabled();
      await draft.setValue(prompt);
      await browser.keys("End");
      await click(panel, "button=Skills & plugin exports");
      await click(panel, `.//*[@role="option"][.//strong[normalize-space(.)="${skillName}"]]`);
      await panel.$(`[aria-label="Selected skills and plugin exports"] code[title="${digest(skillText)}"]`).waitForDisplayed();
      await click(panel, "button=Send");
      let finished;
      await browser.waitUntil(async () => {
        finished = await invoke(browser, "latest_conversation_run", { conversationId: conversation.id });
        return completed.has(finished?.status);
      }, { timeout: fixture ? 45_000 : 180_000, interval: 750, timeoutMsg: "The protected inspector attempt did not settle" });
      assert.equal(finished.status, "succeeded");
      assert.equal(finished.checkoutId, registered.checkoutId);
      const attempt = { conversationId: conversation.id, runId: finished.id };
      const input = await invoke(browser, "conversation_input", attempt);
      assert.equal(input.delivery, "submitted");
      assert.equal(input.historyPayloadDeleted, false);
      const wire = input.frames.map((frame) => JSON.parse(frame)).find(({ method }) => method === "turn/start");
      assert.ok(wire, "The saved dispatch did not contain its exact Codex turn");
      assert.equal(wire.params.input.find(({ type }) => type === "text").text, prompt);
      const skills = wire.params.input.filter(({ type }) => type === "skill");
      assert.equal(skills.length, 1);
      assert.equal(skills[0].name, skillName);
      assert.equal(normalized(skills[0].path), normalized(skillPath));
      const inventory = await invoke(browser, "conversation_dispatch_inventory", attempt);
      assert.equal(inventory.checkout.checkoutId, registered.checkoutId);
      const [skill] = inventory.inventory.requestedSkill.selections;
      assert.equal(skill.revision, digest(skillText));
      assert.equal(normalized(skill.path), normalized(skillPath));
      const instruction = inventory.inventory.instructions.sources.find(({ path }) => normalized(path) === normalized(instructionPath));
      assert.equal(instruction?.revision, digest(instructionText));
      const record = { ...attempt, dispatchId: inventory.dispatchId, checkoutId: registered.checkoutId,
        foreignConversationId: foreign.id, frames: input.frames, skill, instruction };
      assertProtectedRows(database, record, prompt);
      return record;
    } finally {
      await invoke(browser, "set_cli_executable", { provider: "codex", executable: originalCli.executable });
      if (settings.paused) await invoke(browser, "set_job_queue_paused", { paused: true });
      if (originalCheckout) await invoke(browser, "select_active_checkout", { checkoutId: originalCheckout.checkoutId });
    }
  });

  await writeFile(skillPath, `${skillText}\nLater local revision.\n`, "utf8");
  await writeFile(instructionPath, `${instructionText}\nLater local revision.\n`, "utf8");

  await withInstalledWindow(async (browser) => {
    const database = await activeDatabase(browser, invoke, dataRoot);
    const previous = await invoke(browser, "active_checkout");
    try {
      await invoke(browser, "select_active_checkout", { checkoutId: saved.checkoutId });
      await browser.refresh();
      const panel = await openConversation(browser);
      const attempt = { conversationId: saved.conversationId, runId: saved.runId };
      const foreignAttempt = { conversationId: saved.foreignConversationId, runId: saved.runId };
      for (const command of ["conversation_input", "conversation_dispatch_inventory"]) {
        assert.equal(await invoke(browser, command, foreignAttempt), null);
      }
      assert.equal((await invoke(browser, "latest_conversation_run", { conversationId: saved.conversationId })).id, saved.runId);
      const input = await invoke(browser, "conversation_input", attempt);
      assert.deepEqual(input.frames, saved.frames);
      const inventory = await invoke(browser, "conversation_dispatch_inventory", attempt);
      assert.equal(inventory.dispatchId, saved.dispatchId);
      assert.deepEqual(inventory.inventory.requestedSkill.selections, [saved.skill]);
      assert.deepEqual(inventory.inventory.instructions.sources.find(({ path }) => path === saved.instruction.path), saved.instruction);

      await click(panel, "button=Activity");
      const activity = browser.$('[aria-label="Conversation activity"]');
      const exact = activity.$('section[aria-label="Exact provider input"]');
      assert.equal(await exact.isExisting(), false);
      await click(activity, "button=Exact input");
      const frame = exact.$("pre");
      await frame.waitForDisplayed({ timeout: 20_000 });
      assert.equal(await frame.getProperty("textContent"), input.frames.join(""));
      await click(activity, "button=Dispatch inventory");
      const displayed = activity.$('section[aria-label="Saved dispatch inventory"]');
      const selected = displayed.$(skillList);
      await selected.waitForDisplayed({ timeout: 20_000 });
      const evidence = await selected.getText();
      assert.ok(evidence.includes(skillName) && evidence.includes(saved.skill.revision));
      assert.ok(normalized(evidence).includes(normalized(skillPath)));
      await displayed.$(`p=Recorded revision ${saved.instruction.revision}`).waitForDisplayed();
      assertProtectedRows(database, saved, prompt);
    } finally {
      if (previous) await invoke(browser, "select_active_checkout", { checkoutId: previous.checkoutId });
    }
  });

  return { checkoutId: saved.checkoutId, conversationId: saved.conversationId, runId: saved.runId,
    dispatchId: saved.dispatchId, transport: fixture ? "scripted_codex_app_server" : "codex_app_server",
    modelId, exactFramesSha256: digest(saved.frames.join("")), skill: { name: skillName, revision: saved.skill.revision },
    instructionRevision: saved.instruction.revision, restoredAfterRestart: true };
}
