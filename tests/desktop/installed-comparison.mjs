import assert from "node:assert/strict";
import { access, mkdtemp, readFile, realpath, unlink, writeFile } from "node:fs/promises";
import { basename, dirname, isAbsolute, join, relative, sep } from "node:path";

const prompt = "Read the pinned installed comparison fixture";
const modelIds = ["desktop-fixture", "desktop-comparison-blocked", "gpt-6-luna"];
const pinned = "Committed fixture bytes for both candidates.";
const dirty = "Uncommitted bytes remain in the source checkout.";
const terminal = new Set(["succeeded", "failed", "cancelled", "interrupted"]);
const resultsSelector = 'ol[aria-label="Comparison results"]';
const deadline = { timeout: 20_000 };

async function click(scope, selector) {
  const control = scope.$(selector);
  await control.waitForDisplayed(deadline);
  await control.waitForEnabled(deadline);
  await control.click();
}

async function openComparison(browser) {
  await click(browser, 'button[aria-label="Pipelines"]');
  await click(browser, "button=Compare models");
  const workspace = browser.$('section[aria-label="Model comparisons"]');
  await workspace.waitForDisplayed(deadline);
  return workspace;
}

async function chooseCandidates(browser, workspace, options) {
  const trigger = '.model-picker-trigger[aria-label^="Comparison candidates · "]';
  await click(workspace, trigger);
  for (const option of options) {
    const selector = `.model-picker-menu [role="menuitemcheckbox"][aria-label=${JSON.stringify(option.name)}]`;
    await click(browser, selector);
    assert.equal(await browser.$(selector).getAttribute("aria-checked"), "true");
  }
  await browser.keys("Escape");
  await browser.$(".model-picker-menu").waitForExist({ ...deadline, reverse: true });
  const choices = await workspace.$('ol[aria-label="Candidate order"]').$$("strong");
  assert.equal(choices.length, options.length);
  for (let index = 0; index < options.length; index += 1) {
    assert.equal(await choices[index].getText(), options[index].name);
  }
}

async function verifyOutputCards(workspace, commit, options) {
  const saved = workspace.$('section[aria-label="Saved comparison"]');
  await saved.$("h2=Partial result").waitForDisplayed(deadline);
  assert.equal(await saved.$("p*=Pinned Git commit").$("code").getText(), commit);
  const rows = await saved.$$(`${resultsSelector} > li`);
  assert.equal(rows.length, 3);
  for (let index = 0; index < rows.length; index += 1) {
    const row = rows[index];
    assert.equal(await row.$("header h2").getText(), options[index].name);
    assert.equal(await row.$('[role="status"]').getText(), index === 1 ? "failed" : "succeeded");
    const response = row.$('article[aria-label="Original response"]');
    if (index === 1) {
      assert.equal(await response.isExisting(), false);
    } else {
      await response.waitForDisplayed(deadline);
      assert.equal(await response.getText(), `Saved comparison response for ${modelIds[index]}: ${pinned}`);
    }
  }
  return rows;
}

async function comparisonDirectory(dataRoot, path, source) {
  const execution = await realpath(path);
  const storage = await realpath(join(dataRoot, "comparison-checkouts"));
  const location = relative(storage, execution);
  assert.ok(location && !isAbsolute(location) && location !== ".." && !location.startsWith(`..${sep}`),
    "The comparison checkout must be inside the isolated application profile");
  assert.notEqual(execution, source);
  assert.equal(await realpath(join(execution, ".git")), join(execution, ".git"));
  return execution;
}

export async function runInstalledComparison({ application, checkout, repo, dataRoot, withInstalledWindow, invoke, run }) {
  assert.ok(process.env.OMNIAGENT_DESKTOP_FIXTURE_CLI,
    "Set OMNIAGENT_DESKTOP_FIXTURE_CLI to the separately built deterministic comparison CLI");
  const fixture = await realpath(process.env.OMNIAGENT_DESKTOP_FIXTURE_CLI);
  assert.notEqual(fixture, await realpath(application));
  assert.equal(run(fixture, ["login", "status"]), "Logged in to the desktop fixture");
  const source = await realpath(await mkdtemp(join(dirname(repo), "installed-comparison-")));
  const git = (cwd, args) => run("git", args, { cwd });
  const file = "comparison-input.txt";
  const sourceFile = join(source, file);
  const untracked = "origin-only-comparison-note.txt";
  await writeFile(sourceFile, `${pinned}\n`, "utf8");
  await writeFile(join(source, "AGENTS.md"), "# Comparison fixture\nRead the pinned comparison input.\n", "utf8");
  git(source, ["init", "--quiet", "--template="]);
  git(source, ["add", "--", file, "AGENTS.md"]);
  git(source, ["-c", "core.hooksPath=.git/acceptance-hooks", "-c", "user.name=Installer Fixture",
    "-c", "user.email=fixture@example.invalid", "-c", "commit.gpgsign=false", "commit", "-qm", "Pin comparison input"]);
  const commit = git(source, ["rev-parse", "HEAD"]);
  const tree = git(source, ["rev-parse", "HEAD^{tree}"]);
  await writeFile(sourceFile, `${dirty}\n`, "utf8");
  await writeFile(join(source, untracked), "Synthetic work in progress outside the committed input.\n", "utf8");
  const sourceStatus = git(source, ["status", "--porcelain=v1", "--untracked-files=all"]);
  assert.ok(sourceStatus.includes(file) && sourceStatus.includes(untracked));

  const accepted = await withInstalledWindow(async (browser) => {
    const identifier = await invoke(browser, "plugin:app|identifier");
    assert.match(identifier, /^io\.github\.legustavinho\.omniagent\.(?:nsis-test-[0-9a-f]{10}|desktop-tests\.run-[0-9a-f]{32})$/);
    assert.equal(basename(await realpath(dataRoot)), identifier);
    const previousCheckout = await invoke(browser, "active_checkout");
    const previousClis = await invoke(browser, "cli_executables");
    const registered = await invoke(browser, "register_project", { directory: source });
    assert.notEqual(registered.projectId, checkout.projectId);
    const { projectId, checkoutId } = registered;
    const { active, settings } = await invoke(browser, "project_job_queue", { projectId });
    assert.equal(active, 0, "The isolated Windows comparison fixture needs three free slots");
    let release;
    let released = false;
    try {
      await invoke(browser, "select_active_checkout", { checkoutId });
      for (const { provider } of previousClis) {
        await invoke(browser, "set_cli_executable", { provider, executable: fixture });
      }
      await invoke(browser, "set_job_queue_capacity", { capacity: 3 });
      if (settings.paused) await invoke(browser, "set_job_queue_paused", { paused: false });
      const models = await invoke(browser, "conversation_models", { provider: "codex" });
      const options = modelIds.map((id) => models.find(({ option }) => option.selectable && option.selection.modelId === id)?.option);
      assert.ok(options.every(Boolean), "The configured CLI fixture must expose both responders and the blocked model");
      await browser.refresh();
      const workspace = await openComparison(browser);
      const commitInput = workspace.$('input[placeholder="Full commit SHA"]');
      await browser.waitUntil(async () => await commitInput.getValue() === commit, deadline);
      await workspace.$("label=Shared request").$("textarea").setValue(prompt);
      await chooseCandidates(browser, workspace, options);
      assert.deepEqual(await invoke(browser, "model_comparisons", { projectId }), []);
      await click(workspace, "button=Start comparison");

      let group;
      const loadGroup = async () => {
        const groups = await invoke(browser, "model_comparisons", { projectId });
        if (groups.length === 0) return undefined;
        assert.equal(groups.length, 1);
        return groups[0];
      };
      const inventory = (candidate) => invoke(browser, "conversation_dispatch_inventory", {
        conversationId: candidate.conversationId, runId: candidate.runId,
      });
      await browser.waitUntil(async () => {
        group = await loadGroup();
        if (!group) return false;
        assert.equal(group.prompt, prompt);
        assert.deepEqual(group.candidates.map(({ selection }) => selection.modelId), modelIds);
        assert.equal(group.commit, commit);
        assert.equal(group.tree, tree);
        const responses = [group.candidates[0], group.candidates[2]];
        for (const candidate of responses) {
          if (candidate.dispatchId && !release) {
            const cwd = await comparisonDirectory(dataRoot, (await inventory(candidate)).checkout.workingDirectory, source);
            release = join(cwd, ".git", "release-installed-comparison");
          }
        }
        return group.candidates[1].state === "failed" && responses.every(({ state, dispatchId }) => state === "running" && dispatchId);
      }, { timeout: 30_000, interval: 250, timeoutMsg: "Both candidate turns must overlap while the blocked profile settles" });
      const activeCandidates = [group.candidates[0], group.candidates[2]];
      await workspace.$("h2=Comparison in progress").waitForDisplayed(deadline);
      const attempts = await Promise.all(activeCandidates.map(inventory));
      const directories = await Promise.all(attempts.map(({ checkout: saved }) => comparisonDirectory(dataRoot, saved.workingDirectory, source)));
      assert.equal(directories[0], directories[1]);
      assert.notEqual(attempts[0].dispatchId, attempts[1].dispatchId);
      assert.ok(attempts.every(({ checkout: saved }) => saved.checkoutId === checkoutId));
      const directory = directories[0];
      assert.equal(git(directory, ["rev-parse", "HEAD"]), commit);
      assert.equal(git(directory, ["rev-parse", "HEAD^{tree}"]), tree);
      assert.equal(git(directory, ["status", "--porcelain=v1", "--untracked-files=all"]), "");
      assert.equal((await readFile(join(directory, file), "utf8")).trimEnd(), pinned);
      await assert.rejects(access(join(directory, untracked)), { code: "ENOENT" });
      await assert.rejects(access(join(directory, ".git", "objects", "info", "alternates")), { code: "ENOENT" });
      await assert.rejects(invoke(browser, "register_project", { directory }), /isolated|comparison/i);

      assert.equal(release, join(directory, ".git", "release-installed-comparison"));
      await writeFile(release, "", { flag: "wx" });
      released = true;
      await browser.waitUntil(async () => {
        group = await loadGroup();
        return group.candidates.every(({ state }) => terminal.has(state));
      }, { timeout: 30_000, timeoutMsg: "Comparison did not settle after releasing its fixture turns" });
      assert.deepEqual(group.candidates.map(({ state }) => state), ["succeeded", "failed", "succeeded"]);
      const scoped = { projectId, groupId: group.id };
      const outcomes = await invoke(browser, "model_comparison_outcomes", scoped);
      assert.deepEqual(outcomes.map(({ runId }) => runId), group.candidates.map(({ runId }) => runId));
      assert.equal(new Set(outcomes.map(({ runId }) => runId)).size, 3);
      assert.equal(outcomes[1].response, null);
      assert.equal(group.candidates[1].dispatchId, null);
      for (const index of [0, 2]) {
        const { conversationId, dispatchId, runId } = group.candidates[index];
        const history = await invoke(browser, "conversation_history", { conversationId });
        const original = history.messages.find(({ role }) => role === "assistant")?.text;
        assert.equal(original, `Saved comparison response for ${modelIds[index]}: ${pinned}`);
        assert.equal(outcomes[index].response, original);
        const evidence = await invoke(browser, "conversation_profile_evidence", { projectId, conversationId, runId });
        assert.equal(evidence.dispatchId, dispatchId);
        assert.equal(evidence.profile.mode, "read_only");
        assert.equal(evidence.profile.requestedModel, modelIds[index]);
      }
      const rows = await verifyOutputCards(workspace, commit, options);
      await click(rows[1], "button=Open execution");
      const inspector = browser.$(".work-inspector");
      await inspector.$('section[aria-label="Failure record"]').waitForDisplayed(deadline);
      await inspector.$("p=Codex did not establish the requested sandbox").waitForDisplayed(deadline);
      await click(inspector, 'button[aria-label="Close activity"]');

      const writeProbe = join(directory, "local-comparison-isolation-probe.txt");
      await writeFile(writeProbe, "A host test write affects only the disposable comparison checkout.\n", { flag: "wx" });
      try {
        await assert.rejects(access(join(source, basename(writeProbe))), { code: "ENOENT" });
        assert.ok(git(directory, ["status", "--porcelain=v1", "--untracked-files=all"]).includes(basename(writeProbe)));
      } finally {
        await unlink(writeProbe);
      }
      assert.equal(git(directory, ["status", "--porcelain=v1", "--untracked-files=all"]), "");
      assert.equal(await readFile(sourceFile, "utf8"), `${dirty}\n`);
      assert.equal(git(source, ["status", "--porcelain=v1", "--untracked-files=all"]), sourceStatus);
      return { group, options, projectId, checkoutId, directory, outcomes };
    } finally {
      if (release && !released) await writeFile(release, "");
      for (const configuration of previousClis) {
        await invoke(browser, "set_cli_executable", configuration);
      }
      await invoke(browser, "set_job_queue_capacity", { capacity: settings.capacity });
      if (settings.paused) await invoke(browser, "set_job_queue_paused", { paused: true });
      if (previousCheckout) await invoke(browser, "select_active_checkout", { checkoutId: previousCheckout.checkoutId });
    }
  });

  await withInstalledWindow(async (browser) => {
    const previous = await invoke(browser, "active_checkout");
    const previousClis = await invoke(browser, "cli_executables");
    try {
      for (const { provider } of previousClis) {
        await invoke(browser, "set_cli_executable", { provider, executable: fixture });
      }
      await invoke(browser, "select_active_checkout", { checkoutId: accepted.checkoutId });
      await browser.refresh();
      await verifyOutputCards(await openComparison(browser), commit, accepted.options);
      assert.deepEqual(await invoke(browser, "model_comparisons", { projectId: accepted.projectId }), [accepted.group]);
      assert.deepEqual(await invoke(browser, "model_comparison_outcomes", { projectId: accepted.projectId, groupId: accepted.group.id }), accepted.outcomes);
    } finally {
      for (const configuration of previousClis) {
        await invoke(browser, "set_cli_executable", configuration);
      }
      if (previous) await invoke(browser, "select_active_checkout", { checkoutId: previous.checkoutId });
    }
  });

  return { transport: "scripted_codex_app_server", groupId: accepted.group.id, checkoutId: accepted.checkoutId,
    commit, tree, sourceCheckout: source, effectiveComparisonCheckout: accepted.directory, parallelAttempts: 2,
    candidates: accepted.group.candidates.map(({ jobId, runId, state, selection }) => ({ jobId, runId, state, selection })),
    blockedCapability: "read_only_sandbox", sourceUntouched: true, retainedAfterRestart: true };
}
