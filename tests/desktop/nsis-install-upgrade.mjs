import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync } from "node:fs";
import { mkdir, readFile, realpath, writeFile } from "node:fs/promises";
import { createServer } from "node:net";
import { basename, dirname, join, resolve } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { remote } from "webdriverio";

// Run against two release NSIS bundles built from the same source snapshot.
// Transport case.json, package-*.json, installers/ and source/src-tauri/tauri.conf.json
// together to run the exact same installer pair on another Windows target.
// A separately named installed product preserves the production and desktop fixture profiles.
assert.equal(process.platform, "win32");
assert.equal(process.arch, "x64");
const supportedScenarios = ["--workbench", "--backup-restore", "--protected-inspector"];
assert.ok(process.argv[2], `Usage: node tests/desktop/nsis-install-upgrade.mjs <case.json> ${supportedScenarios.map((flag) => `[${flag}]`).join(" ")}`);
const caseFile = resolve(process.argv[2]);
const scenarios = new Set(process.argv.slice(3));
for (const scenario of scenarios) assert.ok(supportedScenarios.includes(scenario), `Unknown NSIS scenario ${scenario}`);
const scratch = dirname(caseFile);
const { product, identifier, sourceCommit, sourcePatch, requireInstallationIdentity = false } = JSON.parse(await readFile(caseFile, "utf8"));
assert.equal(typeof requireInstallationIdentity, "boolean");
const tag = /^OmniAgent NSIS Test ([a-f0-9]{10})$/.exec(product)?.[1];
assert.ok(tag, "A unique NSIS test product name is required");
assert.equal(identifier, `io.github.legustavinho.omniagent.nsis-test-${tag}`);
assert.match(sourceCommit, /^[a-f0-9]{40}$/);
const dataRoot = join(process.env.LOCALAPPDATA, identifier);
const installedRoot = join(process.env.LOCALAPPDATA, product);
const database = join(dataRoot, "omniagent.sqlite3");
const installationIdentity = join(dataRoot, "installation.dpapi");
const uninstallKey = `Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\${product}`;
const bundleRoot = join(scratch, "installers");
const sourceConfig = JSON.parse(await readFile(join(scratch, "source", "src-tauri", "tauri.conf.json"), "utf8"));
assert.deepEqual(sourceConfig.bundle.targets, ["nsis"]);
assert.equal(sourceConfig.bundle.windows.nsis.installMode, "currentUser");
assert.deepEqual(sourceConfig.bundle.windows.webviewInstallMode, { type: "downloadBootstrapper", silent: true });
for (const version of ["0.1.0", "0.1.1"]) {
  const overlay = JSON.parse(await readFile(join(scratch, `package-${version}.json`), "utf8"));
  assert.equal(overlay.productName, product);
  assert.equal(overlay.identifier, identifier);
  assert.equal(overlay.version, version);
  assert.ok(existsSync(join(bundleRoot, `${product}_${version}_x64-setup.exe`)), `Missing NSIS bundle ${version}`);
}

function registry(hive, key, name, view = "64") {
  const result = spawnSync("reg.exe", ["query", `${hive}\\${key}`, "/v", name, `/reg:${view}`], {
    encoding: "utf8", windowsHide: true,
  });
  if (result.error) throw result.error;
  if (result.status !== 0) return undefined;
  return result.stdout.split(/\r?\n/).find((line) => line.trim().startsWith(`${name} `))
    ?.trim().split(/\s+REG_\w+\s+/)[1];
}

function run(executable, args, { cwd = scratch, env = process.env, timeout = 120_000 } = {}) {
  const result = spawnSync(executable, args, { cwd, env, encoding: "utf8", windowsHide: true, timeout });
  if (result.error) throw result.error;
  assert.equal(result.status, 0, `${basename(executable)} exited with ${result.status}\n${result.stderr}`);
  return result.stdout.trim();
}

async function availablePort() {
  const server = createServer();
  await new Promise((accept, reject) => server.once("error", reject).listen(0, "127.0.0.1", accept));
  const { port } = server.address();
  await new Promise((accept, reject) => server.close((error) => error ? reject(error) : accept()));
  return port;
}

async function invoke(browser, name, args = {}) {
  const result = await browser.execute(async (command, values) => {
    try {
      return { data: await window.__TAURI_INTERNALS__.invoke(command, values) };
    } catch (error) {
      return { ipcError: String(error) };
    }
  }, name, args);
  if ("ipcError" in result) throw new Error(result.ipcError);
  return result.data;
}

async function withInstalledWindow(application, verify) {
  const port = await availablePort();
  let nativePort = await availablePort();
  while (nativePort === port) nativePort = await availablePort();
  const driverArgs = ["--port", String(port), "--native-port", String(nativePort)];
  if (process.env.OMNIAGENT_EDGE_DRIVER) driverArgs.push("--native-driver", process.env.OMNIAGENT_EDGE_DRIVER);
  const driver = spawn(process.env.OMNIAGENT_TAURI_DRIVER ?? "tauri-driver", driverArgs, {
    windowsHide: true, stdio: ["ignore", "ignore", "pipe"],
  });
  let driverError;
  let driverLog = "";
  driver.on("error", (error) => { driverError = error; });
  driver.stderr.setEncoding("utf8").on("data", (data) => { driverLog = (driverLog + data).slice(-4096); });
  let browser;
  try {
    for (let attempt = 0; ; attempt++) {
      if (driverError) throw driverError;
      assert.ok(attempt < 60 && driver.exitCode === null, `tauri-driver did not become ready\n${driverLog}`);
      const status = await fetch(`http://127.0.0.1:${port}/status`, { signal: AbortSignal.timeout(500) }).catch(() => null);
      if (status?.ok) break;
      await delay(200);
    }
    browser = await remote({
      hostname: "127.0.0.1", port, path: "/", logLevel: "warn", connectionRetryCount: 0,
      connectionRetryTimeout: 20_000,
      capabilities: { browserName: "webview2", "wdio:enforceWebDriverClassic": true, "tauri:options": { application } },
    });
    await browser.waitUntil(async () => {
      return await invoke(browser, "runtime_status").catch(() => undefined) === "Desktop runtime is ready.";
    }, { timeout: 20_000 });
    assert.equal(await invoke(browser, "runtime_status"), "Desktop runtime is ready.");
    assert.equal(await invoke(browser, "plugin:app|identifier"), identifier);
    assert.equal(await invoke(browser, "plugin:app|bundle_type"), "nsis");
    return await verify(browser);
  } finally {
    await browser?.deleteSession().catch(() => undefined);
    driver.kill();
    // If the WebView closes to the tray, stop only the executable installed for this case.
    run("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", `
      $path = $env:OMNIAGENT_NSIS_TEST_EXE
      Get-CimInstance Win32_Process -Filter "Name='omniagent.exe'" |
        Where-Object { $_.ExecutablePath -eq $path } |
        ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
    `], { env: { ...process.env, OMNIAGENT_NSIS_TEST_EXE: application } });
  }
}

async function sha256(file) {
  return createHash("sha256").update(await readFile(file)).digest("hex");
}

async function verifyBuild(browser, projectId) {
  const { runningBuild } = await invoke(browser, "workflow_maintenance", { projectId });
  assert.equal(runningBuild.sourceCommit, sourceCommit);
  assert.equal(runningBuild.target, "x86_64-pc-windows-msvc");
  assert.equal(runningBuild.profile, "release");
  return runningBuild;
}

async function install(version) {
  const bundle = join(bundleRoot, `${product}_${version}_x64-setup.exe`);
  const installerSha256 = await sha256(bundle);
  run(bundle, ["/S"]);
  assert.equal(registry("HKCU", uninstallKey, "DisplayName"), product);
  assert.equal(registry("HKCU", uninstallKey, "DisplayVersion"), version);
  assert.equal(registry("HKLM", uninstallKey, "DisplayName"), undefined);
  assert.equal(registry("HKLM", uninstallKey, "DisplayName", "32"), undefined);
  const location = registry("HKCU", uninstallKey, "InstallLocation")?.replace(/^"|"$/g, "");
  assert.equal(resolve(location ?? ""), installedRoot);
  const binary = registry("HKCU", uninstallKey, "MainBinaryName");
  assert.equal(binary, "omniagent.exe");
  const application = await realpath(join(location, binary));
  assert.equal(dirname(application), await realpath(installedRoot));
  return { version, bundle, installerSha256, application, installedSha256: await sha256(application) };
}

const windowsKey = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion";
const webviewKey = "SOFTWARE\\Microsoft\\EdgeUpdate\\Clients\\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";
const nativeArchitectures = run("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command",
  "Get-CimInstance Win32_Processor -ErrorAction Stop | Select-Object -ExpandProperty Architecture",
]).split(/\r?\n/);
assert.ok(nativeArchitectures.every((architecture) => architecture === "9"), "A native Windows x64 target is required");
assert.ok(!existsSync(dataRoot) && !existsSync(installedRoot), "NSIS acceptance requires a fresh test product and profile");
assert.equal(registry("HKCU", uninstallKey, "DisplayName"), undefined);
const receipt = {
  sourceCommit, sourcePatch, identifier, product,
  windows: {
    displayVersion: registry("HKLM", windowsKey, "DisplayVersion"),
    build: registry("HKLM", windowsKey, "CurrentBuildNumber"),
    revision: Number(registry("HKLM", windowsKey, "UBR")),
    platform: process.arch,
  },
  webView2: registry("HKLM", webviewKey, "pv", "32") ?? registry("HKCU", webviewKey, "pv"),
  webView2InstallMode: sourceConfig.bundle.windows.webviewInstallMode,
  installationScope: "HKCU",
  database,
};
assert.ok(receipt.webView2, "The WebView2 Evergreen prerequisite must already be present for this case");

const repo = join(scratch, "Documents and worktrees");
await mkdir(repo);
await writeFile(join(repo, "brief.md"), "# Installer acceptance fixture\n", "utf8");
run("git", ["init", "-q"], { cwd: repo });
run("git", ["add", "brief.md"], { cwd: repo });
run("git", ["-c", "core.hooksPath=.git/acceptance-hooks", "-c", "user.name=Installer Fixture", "-c", "user.email=fixture@example.invalid",
  "-c", "commit.gpgsign=false", "commit", "-qm", "Seed acceptance fixture"], { cwd: repo });

try {
  const first = await install("0.1.0");
  const opened = await withInstalledWindow(first.application, async (browser) => {
    assert.equal(await invoke(browser, "plugin:app|version"), first.version);
    await browser.$("button=Open repository").waitForDisplayed({ timeout: 20_000 });
    assert.deepEqual(await invoke(browser, "registered_checkouts"), []);
    const checkout = await invoke(browser, "register_project", { directory: repo });
    return { checkout, build: await verifyBuild(browser, checkout.projectId) };
  });
  const { checkout } = opened;
  assert.equal(await realpath(database), database);
  if (requireInstallationIdentity || existsSync(installationIdentity)) {
    assert.equal(await realpath(installationIdentity), installationIdentity);
    receipt.installationIdentitySha256 = await sha256(installationIdentity);
  }
  receipt.firstInstall = { ...first, ...opened };

  const upgrade = await install("0.1.1");
  assert.equal(upgrade.application, first.application);
  assert.notEqual(upgrade.installedSha256, first.installedSha256);
  const runningUpgrade = await withInstalledWindow(upgrade.application, async (browser) => {
    assert.equal(await invoke(browser, "plugin:app|version"), upgrade.version);
    assert.deepEqual(await invoke(browser, "registered_checkouts"), [checkout]);
    return verifyBuild(browser, checkout.projectId);
  });
  assert.equal(await realpath(database), database);
  if (receipt.installationIdentitySha256) {
    assert.equal(await realpath(installationIdentity), installationIdentity);
    assert.equal(await sha256(installationIdentity), receipt.installationIdentitySha256);
  }
  assert.equal(await readFile(join(repo, "brief.md"), "utf8"), "# Installer acceptance fixture\n");
  receipt.upgrade = { ...upgrade, build: runningUpgrade, restoredCheckoutId: checkout.checkoutId };

  const installed = {
    application: upgrade.application, checkout, repo, scratch, dataRoot, sourceCommit, invoke, run,
    withInstalledWindow: (verify) => withInstalledWindow(upgrade.application, verify),
  };
  if (scenarios.has("--workbench")) {
    const { runInstalledWorkbench } = await import("./installed-workbench.mjs");
    receipt.workbench = await runInstalledWorkbench(installed);
  }
  if (scenarios.has("--backup-restore")) {
    const { runInstalledBackupRestore } = await import("./installed-backup-restore.mjs");
    receipt.backupRestore = await runInstalledBackupRestore(installed);
  }
  if (scenarios.has("--protected-inspector")) {
    const { runInstalledProtectedInspector } = await import("./installed-protected-inspector.mjs");
    receipt.protectedInspector = await runInstalledProtectedInspector(installed);
  }
} finally {
  if (registry("HKCU", uninstallKey, "DisplayName") === product) {
    const uninstaller = await realpath(join(installedRoot, "uninstall.exe"));
    assert.equal(dirname(uninstaller), await realpath(installedRoot));
    run(uninstaller, ["/S"]);
    for (let attempt = 0; attempt < 60 && registry("HKCU", uninstallKey, "DisplayName"); attempt++) {
      await delay(200);
    }
    assert.equal(registry("HKCU", uninstallKey, "DisplayName"), undefined);
    assert.ok(!existsSync(join(installedRoot, "omniagent.exe")));
    receipt.testProductUninstalled = true;
  }
  await writeFile(join(scratch, "install-receipt.json"), JSON.stringify(receipt, null, 2));
}
console.log(JSON.stringify(receipt, null, 2));
