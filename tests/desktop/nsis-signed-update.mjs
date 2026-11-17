import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync } from "node:fs";
import { copyFile, mkdir, readFile, realpath, unlink, writeFile } from "node:fs/promises";
import { createServer } from "node:net";
import { basename, dirname, join, resolve } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { setTimeout as sleep } from "node:timers/promises";
import { remote } from "webdriverio";
import { runAutomatic } from "./nsis-signed-update-automatic.mjs";

assert.equal(process.platform, "win32");
assert.equal(process.arch, "x64");
assert.ok(process.argv[2], "Usage: node tests/desktop/nsis-signed-update.mjs <case.json> [--preflight]");
const caseFile = await realpath(resolve(process.argv[2]));
const root = dirname(caseFile);
const readJson = async (file) => JSON.parse(await readFile(file, "utf8"));
const { inputCommit, tag, identifier, product, automatic = false } = await readJson(caseFile);
assert.match(tag, /^[a-f0-9]{10}$/);
assert.match(inputCommit, /^[a-f0-9]{40}$/);
assert.equal(basename(root), `omniagent-signed-update-${tag}`);
assert.equal(dirname(root).toLowerCase(), (await realpath(process.env.TEMP)).toLowerCase());
assert.equal(identifier, `io.github.legustavinho.omniagent.nsis-update-test-${tag}`);
assert.equal(product, `OmniAgent Signed Update Test ${tag}`);
const local = await realpath(process.env.LOCALAPPDATA);
const appData = join(local, identifier);
const installed = join(local, product);
const application = join(installed, "omniagent.exe");
const publisherKey = `Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\${product}`;
const artifact = (commit) => join(root, "artifacts", commit, "installer.exe");
const fixtureState = join(appData, "fixture-journal.json");
const identityFile = join(appData, "installation.dpapi");
const reportFile = join(root, "update-acceptance-receipt.json");
const plan = await readJson(join(root, "plan.json"));
const { requests, manifests, database, packages, mismatchedManifest } = await readJson(join(root, "signed-case.json"));
const sha = async (file) => createHash("sha256").update(await readFile(file)).digest("hex");

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: root, encoding: "utf8", windowsHide: true, timeout: 90_000, ...options,
  });
  if (result.error) throw result.error;
  return result;
}

function checked(command, args, options) {
  const result = run(command, args, options);
  assert.equal(result.status, 0, `${basename(command)} exited with ${result.status}\n${result.stderr}`);
  return result.stdout.trim();
}

function registry(hive, name, view = "64") {
  const result = run("reg.exe", ["query", `${hive}\\${publisherKey}`, "/v", name, `/reg:${view}`]);
  if (result.status !== 0) return undefined;
  return result.stdout.split(/\r?\n/).find((line) => line.trim().startsWith(`${name} `))
    ?.trim().split(/\s+REG_\w+\s+/)[1].replace(/^"|"$/g, "");
}

function powershell(script, env = {}) {
  return checked("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", script], {
    env: { ...process.env, ...env, OMNIAGENT_FIXTURE_EXE: application },
  });
}

function stopExactApplication() {
  powershell(`Get-CimInstance Win32_Process -Filter "Name='omniagent.exe'" |
    Where-Object { $_.ExecutablePath -eq $env:OMNIAGENT_FIXTURE_EXE } |
    ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }`);
}

function launch(...args) {
  assert.equal(registry("HKCU", "InstallLocation"), installed);
  const process = spawn(application, args, { cwd: installed, stdio: "ignore", windowsHide: true });
  process.unref();
  return process;
}

async function poll(description, operation, timeout = 90_000) {
  const limit = Date.now() + timeout;
  do {
    const value = await operation();
    if (value) return value;
    await sleep(450);
  } while (Date.now() < limit);
  throw new Error(`Timed out waiting for ${description}`);
}

async function journal() {
  if (!existsSync(application) || !existsSync(identityFile)) return null;
  try {
    const result = run(application, ["--read-signed-fixture-journal", fixtureState]);
    return result.status === 0 ? readJson(fixtureState) : null;
  } catch (error) {
    if (["ENOENT", "EACCES", "EBUSY"].includes(error.code)) return null;
    throw error;
  }
}

async function outcome(status, version, prior) {
  return poll(`${version} ${status}`, async () => {
    const entry = await journal();
    if (entry?.update.version !== version || entry.id === prior) return false;
    if (entry.outcome.status === status) return entry;
    if (status === "verified" && ["recovery_required", "install_failed"].includes(entry.outcome.status)) {
      throw new Error(`The initial upgrade failed: ${JSON.stringify(entry)}`);
    }
    return false;
  });
}

async function openPort() {
  const server = createServer();
  await new Promise((accept, reject) => server.once("error", reject).listen(0, "127.0.0.1", accept));
  const { port } = server.address();
  await new Promise((accept, reject) => server.close((error) => error ? reject(error) : accept()));
  return port;
}

async function invoke(browser, name, args = {}) {
  return browser.execute((command, value) => window.__TAURI_INTERNALS__.invoke(command, value), name, args);
}

async function withApp(version, verify, { stop = true } = {}) {
  assert.equal(registry("HKCU", "DisplayVersion"), version);
  const port = await openPort();
  let native = await openPort();
  while (native === port) native = await openPort();
  const args = ["--port", String(port), "--native-port", String(native)];
  if (process.env.OMNIAGENT_EDGE_DRIVER) args.push("--native-driver", process.env.OMNIAGENT_EDGE_DRIVER);
  const driver = spawn(process.env.OMNIAGENT_TAURI_DRIVER ?? "tauri-driver", args, {
    stdio: "ignore", windowsHide: true,
  });
  let fault;
  driver.on("error", (error) => { fault = error; });
  let browser;
  try {
    await poll("WebDriver readiness", async () => {
      if (fault) throw fault;
      if (driver.exitCode !== null) throw new Error(`tauri-driver exited with ${driver.exitCode}`);
      const response = await fetch(`http://127.0.0.1:${port}/status`, { signal: AbortSignal.timeout(500) }).catch(() => null);
      return response?.ok;
    }, 20_000);
    browser = await remote({ hostname: "127.0.0.1", port, path: "/", logLevel: "warn",
      connectionRetryCount: 0, connectionRetryTimeout: 20_000,
      capabilities: { browserName: "webview2", "tauri:options": { application } },
    });
    await poll("installed OmniAgent runtime", async () =>
      await invoke(browser, "runtime_status").catch(() => "") === "Desktop runtime is ready.", 20_000);
    assert.equal(await invoke(browser, "plugin:app|identifier"), identifier);
    assert.equal(await invoke(browser, "plugin:app|bundle_type"), "nsis");
    assert.equal(await invoke(browser, "plugin:app|version"), version);
    return await verify(browser);
  } finally {
    await browser?.deleteSession().catch(() => undefined);
    driver.kill();
    if (stop) stopExactApplication();
  }
}

async function trigger(version, slot, settled) {
  return withApp(version, async (browser) => {
    await browser.execute((request, manifest) => {
      void window.__TAURI_INTERNALS__.invoke("signed_fixture_activate", { request, manifest })
        .catch((error) => { window.__signedFixtureError = String(error); });
    }, requests[slot], manifests[slot]);
    try {
      return await settled();
    } catch (error) {
      const message = await browser.execute(() => window.__signedFixtureError).catch(() => undefined);
      if (message) throw new Error(message, { cause: error });
      throw error;
    }
  }, { stop: false });
}

async function inspectInstalled(version, source, marker) {
  return withApp(version, async (browser) => {
    const { runningBuild, handoffs } = await invoke(browser, "workflow_maintenance", { projectId: requests[0].projectId });
    assert.equal(runningBuild.sourceCommit, source);
    assert.equal(runningBuild.sourceModified, false);
    assert.equal(runningBuild.version, version);
    assert.equal(await invoke(browser, "signed_fixture_migration_state"), marker);
    assert.deepEqual(handoffs.map((handoff) => handoff.id).sort(), requests.map((request) => request.handoffId).sort());
    const signed = handoffs.find((handoff) => handoff.id === requests[0].handoffId)?.signedActivation;
    return { runningBuild, signed, restore: await invoke(browser, "application_restore_status") };
  });
}

assert.match(plan.baselineCommit, /^[a-f0-9]{40}$/);
assert.deepEqual(plan.candidates.map(({ version }) => version), automatic ? ["0.1.1"] : ["0.1.1", "0.1.2"]);
assert.equal(requests.length, plan.candidates.length);
assert.equal(manifests.length, plan.candidates.length);
if (automatic) {
  assert.equal(mismatchedManifest.version, manifests[0].version);
  assert.equal(mismatchedManifest.url, manifests[0].url);
  assert.notEqual(mismatchedManifest.signature, manifests[0].signature);
}
assert.equal(await realpath(database), join(root, "exported", "omniagent.sqlite3"));
assert.equal(await realpath(packages), join(root, "exported", "maintenance-packages"));
for (const { version, commit } of [{ version: "0.1.0", commit: plan.baselineCommit }, ...plan.candidates]) {
  assert.match(commit, /^[a-f0-9]{40}$/);
  const overlay = await readJson(join(root, `package-${version}.json`));
  assert.equal(overlay.identifier, identifier);
  assert.equal(overlay.productName, product);
  assert.equal(overlay.version, version);
  const installer = artifact(commit);
  assert.equal(await realpath(installer), installer);
  const built = await readJson(join(dirname(installer), "built.json"));
  assert.equal(built.source, commit);
  assert.equal(built.version, version);
  assert.equal((await sha(installer)).toUpperCase(), built.sha256);
  assert.equal(powershell("(Get-Item -LiteralPath $env:OMNIAGENT_FIXTURE_INSTALLER).VersionInfo.ProductName",
    { OMNIAGENT_FIXTURE_INSTALLER: installer }), product);
}
for (const [slot, { version, commit }] of plan.candidates.entries()) {
  assert.match(commit, /^[a-f0-9]{40}$/);
  const { handoffId, expectedSha256 } = requests[slot];
  assert.match(handoffId, /^[a-f0-9]{8}(?:-[a-f0-9]{4}){3}-[a-f0-9]{12}$/);
  assert.equal(manifests[slot].version, version);
  assert.equal(manifests[slot].url, "https://release.invalid/nsis-test.exe");
  assert.equal(await sha(join(packages, `${handoffId}.package`)), expectedSha256);
  assert.equal(await sha(artifact(commit)), expectedSha256);
}
assert.ok(!existsSync(appData) && !existsSync(installed), "The disposable profile must be fresh");
assert.equal(registry("HKCU", "DisplayName"), undefined);
assert.equal(registry("HKLM", "DisplayName"), undefined);
assert.equal(registry("HKLM", "DisplayName", "32"), undefined);
assert.equal(powershell("Get-CimInstance Win32_Processor | Select-Object -First 1 -ExpandProperty Architecture"), "9");
const osBuild = powershell("(Get-ItemProperty 'HKLM:\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion').CurrentBuildNumber");
const osBuildRevision = powershell("(Get-ItemProperty 'HKLM:\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion').UBR");
assert.match(osBuildRevision, /^\d+$/);
if (automatic) {
  assert.ok(osBuild === "19045" || (/^\d{5}$/.test(osBuild) && Number(osBuild) >= 22000),
    "The automatic case requires Windows 10 22H2 or Windows 11");
} else {
  assert.equal(osBuild, "19045");
}
const report = { inputCommit, product, identifier, automatic, fixtureData: appData, expectedInstallRoot: installed,
  expectedUninstaller: join(installed, "uninstall.exe"),
  baselineInstallerSha256: await sha(artifact(plan.baselineCommit)), osBuild, osBuildRevision };
const saveReport = () => writeFile(reportFile, JSON.stringify(report, null, 2));
await saveReport();
if (process.argv.includes("--preflight")) {
  console.log(JSON.stringify(report));
  process.exit(0);
}
assert.equal(process.argv.length, 3);

try {
  checked(artifact(plan.baselineCommit), ["/S"]);
  assert.equal(registry("HKCU", "DisplayVersion"), "0.1.0");
  assert.equal(registry("HKCU", "InstallLocation"), installed);
  assert.equal(registry("HKCU", "UninstallString"), join(installed, "uninstall.exe"));
  assert.equal(await realpath(installed), installed);
  assert.equal(await realpath(application), application);
  assert.ok(!existsSync(appData), "The test profile was opened before its seed could be installed");
  await mkdir(join(appData, "maintenance-packages"), { recursive: true });
  await copyFile(database, join(appData, "omniagent.sqlite3"));
  for (const { handoffId } of requests) {
    await copyFile(join(packages, `${handoffId}.package`), join(appData, "maintenance-packages", `${handoffId}.package`));
  }
  report.baseline = await inspectInstalled("0.1.0", plan.baselineCommit, null);
  const identityHash = await sha(identityFile);
  report.installationIdentitySha256 = identityHash;
  await saveReport();
  if (automatic) {
    await runAutomatic({
      root, appData, application, identityFile, identityHash,
      request: requests[0], manifest: manifests[0], mismatchedManifest,
      verifiedSource: plan.candidates[0].commit, report, saveReport,
      withApp, invoke, poll, journal, outcome, registry, sha, stopExactApplication, inspectInstalled,
    });
  } else {
    await runManual(identityHash);
  }
} finally {
  if (registry("HKCU", "DisplayName") === product) {
    stopExactApplication();
    const uninstaller = await realpath(join(installed, "uninstall.exe"));
    assert.equal(uninstaller, join(installed, "uninstall.exe"));
    assert.equal(registry("HKCU", "UninstallString"), uninstaller);
    checked(uninstaller, ["/S"]);
    await poll("uninstallation of the test product", () => !existsSync(application) && !registry("HKCU", "DisplayName"), 30_000);
    report.testProductUninstalled = true;
    await saveReport();
  }
}
console.log(JSON.stringify(report, null, 2));

async function runManual(identityHash) {
  const upgraded = await trigger("0.1.0", 0, () => outcome("verified", "0.1.1"));
  assert.equal(upgraded.sourceBeforeInstall, plan.baselineCommit);
  assert.equal(upgraded.update.sha256, requests[0].expectedSha256);
  assert.equal(await sha(identityFile), identityHash);
  stopExactApplication();
  const installedUpgrade = await inspectInstalled("0.1.1", plan.candidates[0].commit, "migrated");
  assert.equal(installedUpgrade.signed?.id, upgraded.id);
  const previousExecutable = await sha(application);
  report.verifiedUpgrade = { activation: upgraded, installedSha256: previousExecutable, ...installedUpgrade };
  await saveReport();

  const installFlag = join(appData, "fixture-fail-install.flag");
  await writeFile(installFlag, "one failed installer");
  await trigger("0.1.1", 1, () => poll("failure before the NSIS copy section", async () =>
    await readFile(join(appData, "fixture-install-aborted.txt"), "utf8").catch(() => "") === "aborted-before-copy"));
  assert.equal(await sha(application), previousExecutable);
  assert.equal(registry("HKCU", "DisplayVersion"), "0.1.1");
  launch();
  const aborted = await outcome("install_failed", "0.1.2");
  assert.equal(aborted.sourceBeforeInstall, plan.candidates[0].commit);
  report.failedInstall = aborted;
  await saveReport();
  stopExactApplication();
  await unlink(installFlag);

  await writeFile(join(appData, "fixture-fail-startup.flag"), "one failed post-migration startup");
  const failed = await trigger("0.1.1", 1, () => outcome("recovery_required", "0.1.2", aborted.id));
  assert.equal(registry("HKCU", "DisplayVersion"), "0.1.2");
  assert.equal(await readFile(join(appData, "fixture-startup-failed.txt"), "utf8"), "0.1.2");
  assert.notEqual(await sha(application), previousExecutable);
  assert.equal(await sha(identityFile), identityHash);
  const failedDatabase = new DatabaseSync(join(appData, "omniagent.sqlite3"), { readOnly: true });
  let startupCanary;
  try {
    startupCanary = failedDatabase.prepare("SELECT state FROM nsis_fixture_migration").get().state;
  } finally {
    failedDatabase.close();
  }
  assert.equal(startupCanary, "failed-new-build");
  report.failedStartup = { activation: failed, startupCanary };
  await saveReport();

  stopExactApplication();
  launch("--recover-update", failed.id);
  const recovered = await outcome("recovered", "0.1.2");
  assert.equal(recovered.id, failed.id);
  assert.equal(recovered.outcome.previousActivationId, upgraded.id);
  assert.equal(await sha(application), previousExecutable);
  assert.equal(await sha(identityFile), identityHash);
  stopExactApplication();
  const restored = await inspectInstalled("0.1.1", plan.candidates[0].commit, "migrated");
  assert.equal(restored.restore.active?.checkpointId, failed.backup.checkpointId);
  assert.equal(restored.signed?.id, upgraded.id);
  assert.deepEqual(await journal(), recovered);
  report.rollback = { activation: recovered, ...restored };
  await saveReport();
}
