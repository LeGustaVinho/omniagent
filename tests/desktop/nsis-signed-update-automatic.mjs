import assert from "node:assert/strict";
import { existsSync } from "node:fs";
import { readFile, rename, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";

const readJson = async (path) => JSON.parse(await readFile(path, "utf8"));

function invalidSignature(manifest) {
  const lines = Buffer.from(manifest.signature, "base64").toString("utf8").trimEnd().split("\n");
  assert.equal(lines.length, 4);
  const signature = Buffer.from(lines[3], "base64");
  assert.equal(signature.length, 64);
  signature[0] ^= 1;
  lines[3] = signature.toString("base64");
  return { ...manifest, signature: Buffer.from(`${lines.join("\n")}\n`).toString("base64") };
}

function commandState(appData, projectId, jobId) {
  const db = new DatabaseSync(join(appData, "omniagent.sqlite3"), { readOnly: true, timeout: 2_000 });
  try {
    const job = db.prepare("SELECT queue_state FROM jobs WHERE project_id=? AND id=?").get(projectId, jobId);
    assert.ok(job, "The seeded workflow Command disappeared");
    const attempts = db.prepare(`
      SELECT a.ordinal, a.run_id, r.status
      FROM job_attempts a JOIN runs r ON r.id=a.run_id WHERE a.job_id=? ORDER BY a.ordinal
    `).all(jobId).map((row) => ({ ...row }));
    return { pending: job.queue_state, attempts };
  } finally {
    db.close();
  }
}

function recordAdmissionWitness(appData, probeId, handoffId) {
  const db = new DatabaseSync(join(appData, "omniagent.sqlite3"), { timeout: 2_000 });
  try {
    db.exec(`
      CREATE TABLE fixture_probe_admission (
        job_id TEXT PRIMARY KEY,
        handoff_id TEXT NOT NULL,
        run_id TEXT,
        verified_activation_id TEXT
      ) STRICT;
      CREATE TRIGGER fixture_observe_probe_admission AFTER INSERT ON job_attempts
      BEGIN
        UPDATE fixture_probe_admission SET
          run_id = NEW.run_id,
          verified_activation_id = (
            SELECT id FROM workflow_maintenance_signed_activations
            WHERE handoff_id = fixture_probe_admission.handoff_id
          )
        WHERE job_id = NEW.job_id;
      END;
    `);
    db.prepare("INSERT INTO fixture_probe_admission (job_id, handoff_id) VALUES (?, ?)").run(probeId, handoffId);
  } finally {
    db.close();
  }
  return () => {
    const db = new DatabaseSync(join(appData, "omniagent.sqlite3"), { readOnly: true, timeout: 2_000 });
    try {
      return { ...db.prepare(
        "SELECT run_id AS runId, verified_activation_id AS verifiedActivationId FROM fixture_probe_admission WHERE job_id=?",
      ).get(probeId) };
    } finally {
      db.close();
    }
  };
}

export async function runAutomatic({
  root, appData, application, identityFile, identityHash,
  request, manifest, mismatchedManifest, verifiedSource, report, saveReport,
  withApp, invoke, poll, journal, outcome, registry, sha, stopExactApplication, inspectInstalled,
}) {
  const feed = join(appData, "fixture-raw-feed.json");
  const writeFeed = async (value) => {
    await writeFile(`${feed}.tmp`, JSON.stringify(value));
    await rename(`${feed}.tmp`, feed);
  };
  const acceptedPackage = join(appData, "maintenance-packages", `${request.handoffId}.package`);
  const executableBefore = await sha(application);
  const { projectId, holdId, probeId } = await readJson(join(root, "queue-plan.json"));
  assert.equal(projectId, request.projectId);
  assert.notEqual(holdId, probeId);
  const queue = (jobId) => commandState(appData, projectId, jobId);
  // Observe publication in the transaction creating the attempt, before the script can run.
  const readAdmissionWitness = recordAdmissionWitness(appData, probeId, request.handoffId);
  const probeMarker = join(root, "queue-ran.json");
  const target = { projectId, maintenanceSettingsRevision: request.expectedSettingsRevision };
  report.automaticRejections = [];

  await writeFeed(manifest);
  const { upgraded, probe } = await withApp("0.1.0", async (browser) => {
    const settings = () => invoke(browser, "automatic_update_settings");
    const status = (state, matches = () => true) => poll(state, async () => {
      const result = await settings();
      return result.status.state === state && matches(result.status) && result;
    }, 25_000);
    const setPolicy = (expectedRevision, value) =>
      invoke(browser, "set_automatic_update_policy", { expectedRevision, target: value });
    const recordRefusal = async (scenario, observed) => {
      assert.equal(registry("HKCU", "DisplayVersion"), "0.1.0");
      assert.equal(await sha(application), executableBefore);
      assert.equal(await sha(identityFile), identityHash);
      assert.equal(await journal(), null);
      assert.ok(!existsSync(join(appData, "update-backups")));
      assert.deepEqual(queue(probeId), { pending: "paused", attempts: [] });
      report.automaticRejections.push({ scenario, ...observed, activeExecutableSha256: executableBefore });
      await saveReport();
    };

    const disabled = await status("disabled");
    assert.equal(disabled.feedConfigured, true);
    assert.deepEqual(disabled.policy, { revision: null, target: null });
    await recordRefusal("without_grant", disabled);

    await writeFeed(mismatchedManifest);
    const wrongSourceGrant = await setPolicy(null, target);
    const wrongSource = await status("no_matching_handoff");
    assert.equal(wrongSource.policy.revision, wrongSourceGrant.revision);
    assert.equal(wrongSource.status.version, "0.1.1");
    await recordRefusal("signed_with_a_different_source", wrongSource);

    const original = await readFile(acceptedPackage);
    const tampered = Buffer.from(original);
    tampered[tampered.length - 1] ^= 1;
    await writeFile(acceptedPackage, tampered);
    assert.notEqual(await sha(acceptedPackage), request.expectedSha256);
    await writeFeed(manifest);
    const tamperedGrant = await setPolicy(wrongSourceGrant.revision, target);
    const rejected = await status("verification_rejected");
    assert.equal(rejected.policy.revision, tamperedGrant.revision);
    assert.equal(rejected.status.version, "0.1.1");
    assert.match(rejected.status.reason, /installer bytes differ/i);
    await recordRefusal("accepted_bytes_tampered", rejected);

    await writeFile(acceptedPackage, original);
    assert.equal(await sha(acceptedPackage), request.expectedSha256);
    await writeFeed(invalidSignature(manifest));
    const badSignatureGrant = await setPolicy(tamperedGrant.revision, target);
    const invalid = await status("verification_rejected", ({ reason }) => /rejected the installer signature/i.test(reason));
    assert.equal(invalid.policy.revision, badSignatureGrant.revision);
    await recordRefusal("installer_signature_tampered", invalid);

    const revoked = await setPolicy(badSignatureGrant.revision, null);
    assert.equal((await status("disabled")).policy.revision, revoked.revision);
    await writeFeed(manifest);
    await invoke(browser, "set_job_queue_paused", { paused: false });
    const holdMarker = join(root, "queue-hold-started");
    const holdBefore = await poll("the active workflow Command", async () => {
      const { attempts } = queue(holdId);
      if (attempts[0]?.status === "failed") {
        const output = await invoke(browser, "command_job_output", { projectId, runId: attempts[0].run_id });
        throw new Error(`The active workflow Command failed: ${output?.stderr}`);
      }
      return existsSync(holdMarker) && attempts[0]?.status === "running" && attempts;
    }, 25_000);
    assert.equal(holdBefore.length, 1);
    await invoke(browser, "set_pending_job_state", { projectId, jobId: probeId, state: "queued" });
    assert.deepEqual(queue(probeId), { pending: "queued", attempts: [] });

    const authorized = await setPolicy(revoked.revision, target);
    const preparing = await status("preparing_install");
    assert.equal(preparing.status.handoffId, request.handoffId);
    assert.equal(preparing.policy.revision, authorized.revision);
    assert.deepEqual(queue(probeId), { pending: "queued", attempts: [] });
    assert.deepEqual(readAdmissionWitness(), { runId: null, verifiedActivationId: null });
    assert.deepEqual(queue(holdId).attempts, holdBefore);
    assert.ok(!existsSync(probeMarker));
    assert.equal(registry("HKCU", "DisplayVersion"), "0.1.0");
    report.automaticDrain = { active: holdBefore[0], probeBeforeInstall: queue(probeId), preparing };
    await saveReport();
    await writeFile(join(root, "queue-release-hold"), "safe to drain");

    const upgraded = await outcome("verified", "0.1.1");
    assert.equal(upgraded.sourceBeforeInstall, report.baseline.runningBuild.sourceCommit);
    assert.equal(upgraded.update.sha256, request.expectedSha256);
    assert.equal(upgraded.automaticPolicyRevision, authorized.revision);
    const evidence = await poll("the queued Command proving the verified startup", async () => {
      const { attempts } = queue(probeId);
      if (attempts.some(({ status }) => ["failed", "cancelled", "interrupted"].includes(status))) {
        throw new Error(`Queued workflow Command failed: ${JSON.stringify(attempts)}`);
      }
      return attempts[0]?.status === "succeeded" && existsSync(probeMarker) && attempts;
    });
    assert.equal(evidence.length, 1);
    assert.equal(evidence[0].ordinal, 1);
    const admission = readAdmissionWitness();
    assert.deepEqual(admission, { runId: evidence[0].run_id, verifiedActivationId: upgraded.id });
    assert.deepEqual(queue(holdId).attempts, [{ ...holdBefore[0], status: "succeeded" }]);
    const record = await readJson(probeMarker);
    assert.deepEqual(record, { activationId: upgraded.id, version: "0.1.1", sha256: upgraded.update.sha256 });
    return { upgraded, probe: { attempt: evidence[0], admission, record } };
  }, { stop: false });

  assert.equal(await sha(identityFile), identityHash);
  assert.notEqual(await sha(application), executableBefore);
  stopExactApplication();
  const installed = await inspectInstalled("0.1.1", verifiedSource, "migrated");
  assert.equal(installed.signed?.id, upgraded.id);
  report.verifiedAutomaticUpgrade = { activation: upgraded, probe, installedSha256: await sha(application), ...installed };
  await saveReport();
}
