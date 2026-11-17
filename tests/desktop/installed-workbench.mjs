import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { access, mkdir, readFile, realpath, unlink, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";

const sources = ["acceptance/removed-source.ts", "acceptance/retained-source.ts"];
const notes = ["acceptance/First note.md", "acceptance/Second note.md"];
const workspaces = [
  ["Conversations", "conversations"], ["IDE", "ide"], ["Knowledge", "knowledge"],
];

async function click(scope, selector) {
  const control = scope.$(selector);
  await control.waitForDisplayed({ timeout: 15_000 });
  await control.waitForEnabled();
  await control.click();
}

function tabs(browser) {
  return browser.execute(() => {
    const view = document.querySelector(".work-deck:not([hidden]) .work-view:not([hidden])");
    const dock = view?.querySelector(".file-dock, .workspace-dock");
    const label = (tab) => {
      const file = tab?.querySelector(".file-tab-label");
      return file ? { path: file.title, title: file.querySelector("span").textContent }
        : tab?.querySelector(".dv-default-tab-content")?.textContent ?? null;
    };
    return Array.from(dock?.querySelectorAll(".dv-groupview") ?? [], (group) => ({
      tabs: Array.from(group.querySelectorAll(".dv-tab"), label),
      selected: label(group.querySelector(".dv-active-tab")),
      active: group.classList.contains("dv-active-group"),
    }));
  });
}

async function selectWorkspace(browser, label) {
  await click(browser, `button[aria-label=${JSON.stringify(label)}]`);
}

async function openConversation(browser, title) {
  const library = browser.$('section[aria-label="Conversation library"]');
  if (!await library.isDisplayed()) {
    const toggle = browser.$('button[aria-label="Toggle conversation navigation"]');
    if (await toggle.getAttribute("aria-expanded") === "false") await toggle.click();
  }
  await click(library, `button[aria-label=${JSON.stringify(title)}]`);
  const panel = browser.$(`section[aria-label=${JSON.stringify(title)}]`);
  await panel.waitForDisplayed();
  return panel;
}

async function splitFiles(browser, paths, workspace, split) {
  const dock = browser.$(`section[aria-label=${JSON.stringify(workspace + " workspace")}]`);
  for (const path of paths) {
    await click(dock, `button.file-tree-row[title=${JSON.stringify(path)}]`);
    await dock.$(`.file-tab-label[title=${JSON.stringify(path)}]`).waitForDisplayed();
  }
  await click(dock, `button[aria-label=${JSON.stringify(split)}]`);
  assert.deepEqual((await tabs(browser)).map(({ tabs }) => tabs.map(({ path }) => path)), [paths, [paths[1]]]);
}

function nativeWindow(run, application, handle, close = false) {
  return JSON.parse(run("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", `
    Add-Type @'
    using System;
    using System.Runtime.InteropServices;
    public static class OmniAgentFixtureWindow {
        [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr window);
        [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);
        [DllImport("user32.dll", SetLastError = true)] public static extern bool PostMessageW(IntPtr window, uint message, IntPtr wParam, IntPtr lParam);
    }
'@
    $path = $env:OMNIAGENT_NSIS_TEST_EXE
    $owned = @(Get-CimInstance Win32_Process -Filter "Name='omniagent.exe'" | Where-Object { $_.ExecutablePath -eq $path })
    if ($owned.Count -ne 1) { throw 'Expected one executable for the isolated test installation' }
    $app = Get-Process -Id $owned[0].ProcessId -ErrorAction Stop
    $window = $app.MainWindowHandle
    if ($env:OMNIAGENT_NSIS_TEST_HWND) { $window = [IntPtr]::new([long]$env:OMNIAGENT_NSIS_TEST_HWND) }
    [uint32]$owner = 0
    [void][OmniAgentFixtureWindow]::GetWindowThreadProcessId($window, [ref]$owner)
    if ($owner -ne $app.Id) { throw 'Expected the original window of the isolated test installation' }
    if ($env:OMNIAGENT_NSIS_TEST_ACTION -eq 'close') {
      if (![OmniAgentFixtureWindow]::PostMessageW($window, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero)) { throw 'Could not post WM_CLOSE to the fixture window' }
    }
    @{ handle = $window.ToInt64().ToString(); visible = [OmniAgentFixtureWindow]::IsWindowVisible($window) } | ConvertTo-Json -Compress
  `], { env: { ...process.env, OMNIAGENT_NSIS_TEST_EXE: application, OMNIAGENT_NSIS_TEST_HWND: handle ?? "",
    OMNIAGENT_NSIS_TEST_ACTION: close ? "close" : "inspect" } }));
}

export async function runInstalledWorkbench({ application, checkout, repo, withInstalledWindow, invoke, run }) {
  const configuredFixture = process.env.OMNIAGENT_DESKTOP_FIXTURE_CLI;
  assert.ok(configuredFixture, "Set OMNIAGENT_DESKTOP_FIXTURE_CLI to the separately built desktop fixture CLI executable");
  const fixture = await realpath(configuredFixture);
  assert.notEqual(fixture, application);
  assert.equal(run(fixture, ["login", "status"]), "Logged in to the desktop fixture");
  await mkdir(join(repo, "acceptance"));
  await Promise.all([
    ...sources.map((path, i) => writeFile(join(repo, path), `export const acceptance${i} = true;\n`)),
    ...notes.map((path, i) => writeFile(join(repo, path), `# Takeoff note ${i + 1}\n`)),
  ]);
  run("git", ["add", "acceptance"], { cwd: repo });
  run("git", ["-c", "core.hooksPath=.git/acceptance-hooks", "-c", "user.name=Installer Fixture", "-c", "user.email=fixture@example.invalid",
    "-c", "commit.gpgsign=false", "commit", "-qm", "Add tab restore fixtures"], { cwd: repo });

  const titles = ["Saved window operations", "Saved editor operations", "Saved active conversation"];
  const expected = {};
  let activeConversation;
  await withInstalledWindow(async (browser) => {
    await invoke(browser, "select_active_checkout", { checkoutId: checkout.checkoutId });
    await browser.refresh();
    await selectWorkspace(browser, "Conversations");
    for (const title of titles) {
      activeConversation = await invoke(browser, "create_conversation", { checkoutId: checkout.checkoutId, title });
    }
    // Populate real library rows from the persisted checkout when the frontend remounts.
    await browser.refresh();
    await selectWorkspace(browser, "Conversations");
    for (const title of titles) await openConversation(browser, title);
    await click(browser, 'button[aria-label="Split right"]');
    assert.deepEqual((await tabs(browser)).map((group) => group.tabs), [titles.slice(0, 2), titles.slice(2)]);
    expected.conversations = await tabs(browser);

    await selectWorkspace(browser, "IDE");
    await splitFiles(browser, sources, "IDE", "Split editor");
    expected.ide = await tabs(browser);

    await selectWorkspace(browser, "Knowledge");
    await splitFiles(browser, notes, "Knowledge", "Split Reading view");
    expected.knowledge = await tabs(browser);
    for (const [, workspace] of workspaces) {
      assert.equal(expected[workspace].filter(({ active }) => active).length, 1);
      assert.ok(expected[workspace].every(({ selected }) => selected));
      await browser.waitUntil(async () => {
        const layout = await invoke(browser, "workbench_layout", { checkoutId: checkout.checkoutId, workspace });
        return layout && JSON.parse(layout).grid.root.data.length === 2;
      });
    }
  });

  await unlink(join(repo, sources[0]));
  const activeRunId = await withInstalledWindow(async (browser) => {
    assert.equal((await invoke(browser, "active_checkout")).checkoutId, checkout.checkoutId);
    for (const [label, workspace] of workspaces) {
      await selectWorkspace(browser, label);
      await browser.waitUntil(async () => JSON.stringify(await tabs(browser)) === JSON.stringify(expected[workspace]), {
        timeout: 15_000, timeoutMsg: `Tabs, groups and selection for ${label} did not restore`,
      });
    }
    await selectWorkspace(browser, "IDE");
    await click(browser, `.dv-tab:has(.file-tab-label[title=${JSON.stringify(sources[0])}])`);
    const unavailable = browser.$('section[aria-label="IDE workspace"] .file-unavailable');
    await unavailable.$("p=The file is missing from this checkout.").waitForDisplayed();
    assert.equal(await unavailable.$("code").getText(), sources[0]);

    await invoke(browser, "set_cli_executable", { provider: "codex", executable: fixture });
    await browser.refresh();
    await selectWorkspace(browser, "Conversations");
    const panel = await openConversation(browser, activeConversation.title);
    await click(panel, ".model-picker-trigger");
    await click(browser, '.model-picker-menu [role="menuitemradio"][aria-label="Desktop fixture"]');
    const scope = { conversationId: activeConversation.id };
    const latest = () => invoke(browser, "latest_conversation_run", scope);
    const release = join(repo, "release-desktop-jobs");
    try {
      await panel.$(`textarea[id="draft-${activeConversation.id}"]`).setValue("Hold desktop slot 1");
      await click(panel, "button=Send");
      await browser.waitUntil(() => access(join(repo, "desktop-slot-1")).then(() => true, () => false), { timeout: 30_000 });
      const active = await latest();
      assert.equal(active.status, "running");
      await panel.$("button=Stop").waitForDisplayed();
      const page = await browser.getUrl();
      const window = nativeWindow(run, application);
      assert.equal(window.visible, true);
      nativeWindow(run, application, window.handle, true);
      await browser.waitUntil(() => !nativeWindow(run, application, window.handle).visible, { timeout: 15_000 });
      await browser.navigateTo("about:blank");
      run(application, [], { cwd: dirname(application), timeout: 10_000 });
      await browser.waitUntil(() => nativeWindow(run, application, window.handle).visible, { timeout: 15_000 });
      await browser.navigateTo(page);
      await selectWorkspace(browser, "Conversations");
      const reopened = await openConversation(browser, activeConversation.title);
      await reopened.$("button=Stop").waitForDisplayed();
      assert.equal((await latest()).id, active.id);
      assert.equal((await latest()).status, "running");
      await writeFile(release, "");
      await browser.waitUntil(async () => (await latest())?.status === "succeeded", { timeout: 30_000 });
      assert.equal((await latest()).id, active.id);
      return active.id;
    } finally {
      await writeFile(release, "");
    }
  });

  const fixtureSha256 = createHash("sha256").update(await readFile(fixture)).digest("hex");
  return { tabs: expected, missingPath: sources[0], activeRunId, fixtureCli: { executable: fixture, sha256: fixtureSha256 } };
}
