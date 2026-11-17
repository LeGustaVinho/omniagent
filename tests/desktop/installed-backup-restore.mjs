import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdir, readFile, readdir, realpath, writeFile } from "node:fs/promises";
import { basename, join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

const exec = promisify(execFile);
const script = (name) => fileURLToPath(new URL(name, import.meta.url));
const digest = (bytes) => createHash("sha256").update(bytes).digest("hex");
const timeout = 25_000;
const lateNote = "Note discovered after checkpoint.md";
const newerFile = "Live worktree content retained after restoring the checkpoint.\n";
const newQuestionTimeout = 18;

async function click(element) {
  await element.waitForDisplayed({ timeout });
  await element.waitForEnabled({ timeout });
  await element.click();
}

async function openBackupSettings(browser) {
  await click(browser.$('button[aria-label="Settings"]'));
  await click(browser.$('nav[aria-label="Settings categories"]').$("button=System"));
}

async function chooseFolder(application, button, title, directory) {
  await Promise.all([
    exec("powershell.exe", ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File",
      script("./installed-backup-restore-picker.ps1"), "-Application", application, "-Title", title, "-Directory", directory],
    { windowsHide: true, timeout: timeout + 5000 }),
    click(button),
  ]);
}

async function verifyArchive(archive, seed) {
  const manifestBytes = await readFile(join(archive, "manifest.json"));
  const manifest = JSON.parse(manifestBytes.toString("utf8"));
  const manifestSha256 = digest(manifestBytes);
  const image = await readFile(join(archive, "omniagent.sqlite3"));
  assert.equal(digest(image), manifest.settingsRevision);
  assert.equal(manifest.entries.find((entry) => entry.kind === "database").sha256, digest(image));
  assert.ok(!image.includes(Buffer.from(seed.promptMarker)), "The backup exposed the protected prompt as plaintext");
  assert.ok(!image.includes(Buffer.from(seed.diffMarker)), "The backup exposed the protected patch as plaintext");
  return { checkpointId: manifest.checkpointId, manifestSha256 };
}

async function historicalEvidence(browser, invoke, seed) {
  const { projectId, checkoutId } = seed.checkout;
  const taskScope = { projectId, taskId: seed.taskId };
  const runScope = { projectId, runId: seed.runId };
  const conversationScope = { conversationId: seed.conversationId, runId: seed.runId };
  const hierarchy = await invoke(browser, "project_work_hierarchy", { projectId });
  assert.ok(hierarchy.work.some(({ task }) => task.id === seed.taskId && task.title === seed.taskTitle));
  const attempts = await invoke(browser, "project_task_runs", taskScope);
  const attempt = attempts.find(({ runId }) => runId === seed.runId);
  assert.equal(attempt?.conversationId, seed.conversationId);
  const exact = await invoke(browser, "conversation_input", conversationScope);
  assert.equal(exact.delivery, "submitted");
  assert.equal(exact.historyPayloadDeleted, false);
  assert.equal(digest(exact.frames.join("")), seed.promptSha256);
  assert.ok(exact.frames.join("").includes(seed.promptMarker));
  const evidence = await invoke(browser, "attempt_worktree_evidence", runScope);
  assert.equal(evidence.kind, "saved");
  assert.equal(evidence.artifact.revision, seed.captureRevision);
  const diff = Buffer.from(evidence.artifact.candidate.changes.unstagedPatchBase64, "base64");
  assert.equal(digest(diff), seed.diffSha256);
  assert.ok(diff.toString("utf8").includes(seed.diffMarker));
  const source = await invoke(browser, "read_repository_markdown", { checkoutId, revisionId: seed.sourceRevision });
  assert.equal(source.revision.id, seed.sourceRevision);
  assert.equal(source.revision.relativePath, "brief.md");
  assert.equal(source.markdown, seed.sourceMarkdown);
  return attempt;
}

async function showRestoredTask(browser, seed, attempt) {
  await click(browser.$('button[aria-label="Tasks"]'));
  const work = browser.$('section[aria-label="Tasks workspace"]');
  await click(work.$(`button=${seed.taskTitle}`));
  const detail = work.$('aside[aria-label="Work detail"]');
  await detail.$(`h3=${seed.taskTitle}`).waitForDisplayed({ timeout });
  assert.match(attempt.role.value, /^[\w -]+$/);
  const attemptButton = detail.$(`.//li[./div/p[starts-with(normalize-space(.), '${attempt.role.value} · ')]]//button[normalize-space(.)='Open attempt ${attempt.ordinal}']`);
  await click(attemptButton);
  const inspector = browser.$('aside[aria-label="Conversation activity"]');
  await browser.waitUntil(async () => (await inspector.getText()).includes(seed.runId), { timeout, timeoutMsg: "The task did not open the saved attempt" });
  await click(inspector.$("button=Exact input"));
  const input = inspector.$('section[aria-label="Exact provider input"] pre');
  await input.waitForDisplayed({ timeout });
  assert.ok((await input.getText()).includes(seed.promptMarker), "The historical prompt did not render in the task attempt");
  await click(browser.$('button[aria-label="Close activity"]'));
}

async function showRestoredDiff(browser, seed) {
  await click(browser.$('button[aria-label="Git"]'));
  await click(browser.$("button=Saved attempts"));
  const select = browser.$('//label[contains(., "Recorded attempt")]/select');
  await select.waitForEnabled({ timeout });
  await select.selectByAttribute("value", seed.runId);
  const identity = browser.$('dl[aria-label="Recorded comparison identity"]');
  await browser.waitUntil(async () => (await identity.getText()).includes(seed.captureRevision), { timeout });
  const diffSource = browser.$('//label[contains(., "Recorded diff source")]/select');
  await diffSource.selectByVisibleText("Captured working changes");
  await browser.waitUntil(async () => (await browser.$('.monaco-editor .view-lines').getText()).includes(seed.diffMarker),
    { timeout, timeoutMsg: "The historical managed patch did not render in Git" });
}

async function rebuildAndFollowHistoricalLink(browser, invoke, seed) {
  const { checkoutId } = seed.checkout;
  await click(browser.$('button[aria-label="Knowledge"]'));
  await click(browser.$('button[aria-label="Rebuild note index"]'));
  await browser.waitUntil(async () => Boolean(await invoke(browser, "repository_indexed_note", { checkoutId, path: lateNote })),
    { timeout, timeoutMsg: "The restored checkout index did not discover the new on-disk note" });
  const edges = await invoke(browser, "repository_note_edges", { checkoutId });
  assert.ok(edges.some((edge) => edge.sourcePath === "brief.md" && edge.targetPath === "Review target.md"));
  const links = await invoke(browser, "repository_note_relationships", { checkoutId, path: "brief.md" });
  const historical = links.outgoing.find(({ resolution }) => resolution.status === "resolved" && resolution.location.path === "Review target.md");
  assert.equal(historical.resolution.location.anchor, "Decision");
  const navigation = browser.$('aside[aria-label="Knowledge navigation"]');
  if (!await navigation.isDisplayed()) await click(browser.$('button[aria-label="Toggle Knowledge navigation"]'));
  await click(navigation.$("button=brief.md"));
  await click(browser.$("button=Reading"));
  await click(browser.$("button=Historical decision"));
  await browser.$('//div[contains(@class, "ide-file-caption")]//span[normalize-space(.)="Review target.md"]').waitForDisplayed({ timeout });
  return historical.resolution.location;
}

export async function runInstalledBackupRestore({ application, scratch, dataRoot, sourceCommit, withInstalledWindow, invoke }) {
  await exec("pwsh.exe", ["-NoProfile", "-File", script("./installed-backup-restore-seed.ps1"), "-Case", scratch],
    { windowsHide: true, timeout: 20 * 60 * 1000 });
  console.log("Seeded the installed profile with protected task evidence");
  const seed = JSON.parse(await readFile(join(scratch, "backup-restore-seed.json"), "utf8"));
  const { projectId, checkoutId, checkoutRoot } = seed.checkout;
  const identityPath = join(dataRoot, "installation.dpapi");
  const identityHash = digest(await readFile(identityPath));
  const backupParent = join(scratch, "restore-checkpoints");
  await mkdir(backupParent);
  const historicalTask = { projectId, taskId: seed.taskId };
  const checkpoint = await withInstalledWindow(async (browser) => {
    assert.equal((await invoke(browser, "active_checkout")).checkoutId, checkoutId);
    await historicalEvidence(browser, invoke, seed);
    assert.equal(await invoke(browser, "project_question_timeout", { projectId }), seed.questionTimeout);
    assert.equal(await invoke(browser, "repository_indexed_note", { checkoutId, path: lateNote }), null);

    await openBackupSettings(browser);
    await chooseFolder(application, browser.$("button=Create backup"), "Choose where to save the backup", backupParent);
    await browser.$("p*=Verified backup saved to").waitForDisplayed({ timeout });
    const archives = (await readdir(backupParent)).filter((name) => /^OmniAgent-backup-[a-f0-9-]+$/.test(name));
    assert.equal(archives.length, 1);
    const archive = await realpath(join(backupParent, archives[0]));
    const receipt = await verifyArchive(archive, seed);
    console.log(`Created and verified installed checkpoint ${receipt.checkpointId}`);

    const target = { kind: "attempt_diff", conversationId: seed.conversationId, runId: seed.runId };
    const mode = "with_derived_copies";
    const deletion = await invoke(browser, "preview_conversation_history_deletion", { target, mode });
    assert.ok(deletion.attemptDiffIds.includes(seed.runId));
    await invoke(browser, "confirm_conversation_history_deletion", { target, mode, expectedRevision: deletion.revision });
    assert.equal((await invoke(browser, "attempt_worktree_evidence", { projectId, runId: seed.runId })).kind, "unavailable");
    await invoke(browser, "set_project_question_timeout", { projectId, minutes: newQuestionTimeout });
    await writeFile(join(seed.worktree, "first.txt"), newerFile);
    await writeFile(join(checkoutRoot, lateNote), "# Found on disk after refreshing the restored index\n");

    await chooseFolder(application, browser.$("button=Preview restore"), "Choose an OmniAgent backup folder", archive);
    const stage = browser.$("button=Stage restore for next restart");
    await stage.waitForDisplayed({ timeout });
    assert.ok((await browser.$('section[data-setting="backup"]').getText()).includes(receipt.checkpointId));
    await click(stage);
    await browser.$("p*=Restore staged.").waitForDisplayed({ timeout });
    const status = await invoke(browser, "application_restore_status");
    assert.equal(status.pending.checkpointId, receipt.checkpointId);
    assert.equal(status.active, null);
    assert.equal(await invoke(browser, "project_question_timeout", { projectId }), newQuestionTimeout);
    console.log(`Staged checkpoint ${receipt.checkpointId} for the next launch`);
    return { ...receipt, archive: basename(archive), generationId: status.pending.generationId };
  });
  const historicalLink = await withInstalledWindow(async (browser) => {
    const status = await invoke(browser, "application_restore_status");
    assert.equal(status.rejection, null);
    assert.equal(status.pending, null);
    assert.equal(status.active.checkpointId, checkpoint.checkpointId);
    assert.equal(status.active.generationId, checkpoint.generationId);
    assert.equal(digest(await readFile(identityPath)), identityHash);
    assert.equal(await invoke(browser, "project_question_timeout", { projectId }), seed.questionTimeout);
    assert.equal(await readFile(join(seed.worktree, "first.txt"), "utf8"), newerFile);
    assert.equal(await invoke(browser, "repository_indexed_note", { checkoutId, path: lateNote }), null);
    const attempt = await historicalEvidence(browser, invoke, seed);
    await showRestoredTask(browser, seed, attempt);
    await showRestoredDiff(browser, seed);
    return rebuildAndFollowHistoricalLink(browser, invoke, seed);
  });
  console.log(`Reopened task ${seed.taskId} and verified its saved prompt, settings, diff, and note links`);

  return { sourceCommit, checkpoint, historicalTask, runId: seed.runId, promptSha256: seed.promptSha256,
    questionTimeout: seed.questionTimeout, captureRevision: seed.captureRevision, diffSha256: seed.diffSha256,
    sourceRevision: seed.sourceRevision, historicalLink, worktreeWasPreserved: true, indexedNewNote: lateNote };
}
