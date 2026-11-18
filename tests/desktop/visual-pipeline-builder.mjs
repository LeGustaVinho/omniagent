import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { createHash, randomUUID } from "node:crypto";
import { realpath, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { Key } from "webdriverio";

const exec = promisify(execFile);
const timeout = 20_000;
const builder = '[aria-label="Pipeline graph editor"]';
const canvas = '[aria-label="Pipeline graph canvas"]';
const start = "start";
const wait = "wait";
const end = "end";
const waitNode = `.react-flow__node[data-id="${wait}"]`;
const waitReady = makeWire(start, "start", wait, "ready");
const waitInput = makeWire(start, "inputs", wait, "input");
const endReady = makeWire(wait, "completed", end, "ready");
const endResult = makeWire(wait, "result", end, "result");
const wires = [waitReady, waitInput, endReady, endResult];
const wireLabel = ({ from, to }) => `${from.node}.${from.port} to ${to.node}.${to.port}`;
const viewports = [[1440, 900], [1120, 760], [640, 440]];

function makeWire(node, port, target, input) {
  return { from: { node, port }, to: { node: target, port: input } };
}

async function click(element) {
  await element.waitForDisplayed({ timeout });
  await element.waitForEnabled({ timeout });
  await element.click();
}

async function select(scope, label, value) {
  const field = scope.$(`.//label[starts-with(normalize-space(.), '${label}')]/select`);
  await field.waitForEnabled({ timeout });
  await field.selectByAttribute("value", value);
  return field;
}

async function textIsVisible(browser, element, text) {
  await browser.waitUntil(async () => await element.isDisplayed() && (await element.getText()).includes(text),
    { timeout, timeoutMsg: `Expected visible pipeline evidence: ${text}` });
}

async function replaceText(browser, field, value) {
  await click(field);
  await browser.action("key").down(Key.Control).down("a").up("a").up(Key.Control).perform();
  await field.addValue(value);
  assert.equal(await field.getValue(), value);
}

async function locate(browser, selector) {
  let point;
  await browser.waitUntil(async () => {
    point = await browser.execute((path) => {
      const element = document.querySelector(path);
      if (!element) return null;
      const rect = element.getBoundingClientRect();
      const x = Math.round(rect.left + rect.width / 2);
      const y = Math.round(rect.top + rect.height / 2);
      return rect.width > 0 && rect.height > 0 && x >= 0 && y >= 0 && x < innerWidth && y < innerHeight
        && element.contains(document.elementFromPoint(x, y)) ? { x, y } : null;
    }, selector);
    return point !== null;
  }, { timeout, timeoutMsg: `The canvas control is not reachable by the pointer: ${selector}` });
  return point;
}

async function drag(browser, from, to) {
  const intermediate = { x: Math.round((from.x + to.x) / 2), y: Math.round((from.y + to.y) / 2) };
  await browser.performActions([{ type: "pointer", id: "builder-pointer", parameters: { pointerType: "mouse" }, actions: [
    { type: "pointerMove", origin: "viewport", duration: 0, ...from },
    { type: "pointerDown", button: 0 },
    { type: "pause", duration: 50 },
    { type: "pointerMove", origin: "viewport", duration: 80, ...intermediate },
    { type: "pointerMove", origin: "viewport", duration: 125, ...to },
    { type: "pause", duration: 80 },
    { type: "pointerUp", button: 0 },
  ] }]);
  await browser.releaseActions();
}

async function pressEnter(browser, selector) {
  const element = browser.$(selector);
  await element.waitForDisplayed({ timeout });
  await element.waitForEnabled({ timeout });
  assert.ok(await browser.execute((path) => {
    const button = document.querySelector(path);
    button.focus();
    return window.document.activeElement === button;
  }, selector), "The keyboard connection control did not receive focus");
  await browser.keys("Enter");
}

const handle = (node, port) => `${canvas} .react-flow__handle[data-nodeid="${node}"][data-handleid="${port}"]`;
const edge = (wire) => `${canvas} .react-flow__edge[aria-label="${wireLabel(wire)}"]`;

function assertWires(savedWires) {
  assert.deepEqual(savedWires.map(wireLabel).sort(), wires.map(wireLabel).sort());
}

async function connectByKeyboard(browser, { from, to }) {
  await pressEnter(browser, `${builder} button[aria-label="Connect from ${from.node}.${from.port}"]`);
  await pressEnter(browser, `${builder} button[aria-label="Connect to ${to.node}.${to.port}"]`);
}

async function nodePosition(browser, surface = canvas) {
  return browser.execute((selector) => {
    const element = window.document.querySelector(selector);
    if (!element) return null;
    const transform = new DOMMatrixReadOnly(getComputedStyle(element).transform);
    return { x: transform.m41, y: transform.m42 };
  }, `${surface} ${waitNode}`);
}

function closePosition(actual, expected) {
  assert.ok(actual && expected, "The Wait node must have a saved position");
  assert.ok(Math.abs(actual.x - expected.x) < 1 && Math.abs(actual.y - expected.y) < 1,
    `The restored canvas position differs from the saved position: ${JSON.stringify({ actual, expected })}`);
}

async function chooseMarkdown(application, button, file) {
  const temp = await realpath(tmpdir());
  await Promise.all([
    exec("powershell.exe", ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File",
      fileURLToPath(new URL("./installed-backup-restore-picker.ps1", import.meta.url)),
      "-Application", application, "-Title", "Select repository Markdown", "-File", file],
    { windowsHide: true, timeout: timeout + 10_000, env: { ...process.env, TEMP: temp, TMP: temp } }),
    click(button),
  ]);
}

async function openBuilder(browser) {
  await click(browser.$('button[aria-label="Pipelines"]'));
  await click(browser.$('nav[aria-label="Pipelines views"]').$("button=Builder"));
}

async function setTheme(browser, invoke, theme) {
  await click(browser.$('button[aria-label="Settings"]'));
  await click(browser.$("button=Appearance"));
  const button = browser.$('[data-setting="theme"]').$(theme === "light" ? "button=Light" : "button=Dark");
  await click(button);
  await browser.waitUntil(async () => (await invoke(browser, "appearance_settings")).theme === theme,
    { timeout, timeoutMsg: `The ${theme} theme was not saved in the disposable profile` });
  await click(browser.$('button[aria-label="Close settings"]'));
}

async function setWindow(browser, invoke, width, height) {
  const size = { width, height };
  await invoke(browser, "plugin:window|set_size", { label: "main", value: { Logical: size } });
  await browser.waitUntil(() => browser.execute((wanted) =>
    Math.abs(innerWidth - wanted.width) <= 4 && Math.abs(innerHeight - wanted.height) <= 4, size),
  { timeout, timeoutMsg: `Window did not reach ${width}×${height}` });
}

async function fitGraph(browser, surface) {
  await browser.execute((selector) => document.querySelector(selector)?.scrollIntoView({ block: "center", inline: "nearest" }), surface);
  await click(browser.$(`${surface} .react-flow__controls-fitview`));
  let cards;
  try {
    await browser.waitUntil(async () => {
      cards = await browser.execute((selector, ids) => {
        const graph = document.querySelector(selector);
        const graphRect = graph.getBoundingClientRect();
        return ids.map((id) => {
          const header = graph.querySelector(`.react-flow__node[data-id="${id}"] header`);
          if (!header) return { id, visible: false };
          const rect = header.getBoundingClientRect();
          const x = Math.round(rect.left + rect.width / 2);
          const y = Math.round(rect.top + rect.height / 2);
          const hit = document.elementFromPoint(x, y);
          return { id, visible: header.contains(hit), x, y, graphTop: graphRect.top, graphHeight: graphRect.height,
            topElement: hit?.className?.toString() };
        });
      }, surface, [start, wait, end]);
      return cards.every(({ visible }) => visible);
    }, { timeout });
  } catch (error) {
    throw new Error(`Fit view did not reveal all three cards in ${surface}: ${JSON.stringify(cards)}`, { cause: error });
  }
}

async function verifyResponsiveView(browser, invoke, capture, emulateDisplay, { name, canvas: graph, inspector, nodeLabel }) {
  async function atEachSize(theme) {
    for (const [width, height] of viewports) {
      const summary = `${name}, ${theme}, ${width}×${height}`;
      await setWindow(browser, invoke, width, height);
      const panel = browser.$(inspector);
      const compactToggle = `${graph} + button.pipeline-compact-inspector`;
      if (width === 640) {
        await panel.waitForDisplayed({ timeout, reverse: true });
        await textIsVisible(browser, browser.$(compactToggle), "Show inspector");
        await pressEnter(browser, compactToggle);
      }
      await select(panel, nodeLabel, wait);
      await textIsVisible(browser, panel.$(".pipeline-inspector-scroll > header:first-child h2"), "Wait");
      if (width === 640) await capture?.(browser, `${name}-${theme}-${width}x${height}-inspector`);

      const geometry = await browser.execute((graphSelector, inspectorSelector) => {
        const graphElement = document.querySelector(graphSelector);
        const aside = document.querySelector(inspectorSelector);
        const frame = graphElement.parentElement;
        const canvasRect = graphElement.getBoundingClientRect();
        const inspectorRect = aside.getBoundingClientRect();
        const selected = graphElement.querySelector(".workflow-node-card[data-selected]");
        return {
          documentOverflow: Math.max(document.documentElement.scrollWidth, document.body.scrollWidth) - innerWidth,
          frameOverflow: frame.scrollWidth - frame.clientWidth,
          graphWidth: canvasRect.width,
          graphHeight: canvasRect.height,
          inspectorWidth: inspectorRect.width,
          inspectorHeight: inspectorRect.height,
          selectedOutline: selected ? getComputedStyle(selected).outlineStyle : null,
        };
      }, graph, inspector);
      assert.ok(geometry.documentOverflow <= 1 && geometry.frameOverflow <= 1,
        `Horizontal overflow in ${summary}: ${JSON.stringify(geometry)}`);
      assert.ok(geometry.graphWidth >= 300 && geometry.graphHeight >= 225 &&
        geometry.inspectorWidth >= 220 && geometry.inspectorHeight >= 150,
        `Canvas or inspector became unusably small in ${summary}: ${JSON.stringify(geometry)}`);
      if (theme === "high-contrast") assert.equal(geometry.selectedOutline, "solid", `Selected node outline in ${summary}`);
      if (width === 640) {
        await browser.keys("Escape");
        await panel.waitForDisplayed({ timeout, reverse: true });
      }
      await fitGraph(browser, graph);
      await capture?.(browser, `${name}-${theme}-${width}x${height}`);
    }
  }
  for (const theme of ["light", "dark"]) {
    await setTheme(browser, invoke, theme);
    await browser.$(`${graph} .react-flow.${theme}`).waitForDisplayed({ timeout });
    await atEachSize(theme);
  }
  await emulateDisplay(browser, "Emulation.setEmulatedMedia", { features: [{ name: "forced-colors", value: "active" }] });
  try {
    assert.equal(await browser.execute(() => matchMedia("(forced-colors: active)").matches), true);
    await atEachSize("high-contrast");
  } finally {
    await emulateDisplay(browser, "Emulation.setEmulatedMedia", { features: [] });
  }
  await setWindow(browser, invoke, 1120, 760);
}

async function reviewSavedRevision(browser, saved) {
  await click(browser.$("button=Review changes"));
  const review = browser.$('[aria-label="Review workflow package"]');
  const revision = review.$(".//label[starts-with(normalize-space(.), 'Review saved revision')]/select");
  await revision.waitForDisplayed({ timeout });
  assert.equal(await revision.getValue(), saved.revision);
  await review.$("summary=Changes and dependencies").waitForDisplayed({ timeout });
  return review;
}

async function launchFromTask({ browser, invoke, capture, checkout, title, version, sourceRevision }) {
  const { projectId, checkoutId } = checkout;
  const listRuns = () => invoke(browser, "list_repository_deliveries", { projectId });
  const previousRuns = new Set((await listRuns()).map(({ id }) => id));
  await click(browser.$('button[aria-label="Tasks"]'));
  await click(browser.$("button=New work"));
  const dialog = browser.$('[role="dialog"]');
  await select(dialog, "Work type", "task");
  await replaceText(browser, dialog.$(".//label[normalize-space(.)='Title']/input"), title);
  await click(dialog.$("button=Create work"));

  const detail = browser.$('[aria-label="Work detail"]');
  await textIsVisible(browser, detail.$("h3"), title);
  const { work } = await invoke(browser, "project_work_hierarchy", { projectId });
  const task = work.find(({ task }) => task.title === title);
  assert.equal(task.placement.kind, "task");
  await click(detail.$('[aria-label="Task detail views"]').$("button=Workflow"));
  const launch = detail.$('[aria-label="Launch workflow"]');
  await click(launch.$("button=Run published workflow"));
  await select(launch, "Published workflow revision", JSON.stringify([version.id, version.revision]));
  const inputs = launch.$('[aria-label="Start inputs"]');
  await select(inputs, "Checkout", checkoutId);
  await textIsVisible(browser, inputs.$(".pipeline-meta"), title);
  await click(inputs.$(".//label[contains(normalize-space(.),'Include a Markdown revision')]/input"));
  const source = inputs.$('[aria-label="Repository Markdown source"]');
  await select(source, "Saved source revision", sourceRevision.id);
  await click(source.$("button=Start workflow"));

  let run;
  await browser.waitUntil(async () => {
    run = (await listRuns()).find(({ id, version: pin }) => !previousRuns.has(id) &&
      pin.id === version.id && pin.revision === version.revision);
    return Boolean(run);
  }, { timeout, timeoutMsg: "The Task did not launch the selected workflow revision" });
  const execution = browser.$('[aria-label="Pinned workflow execution"]');
  await textIsVisible(browser, execution, version.revision);
  assert.equal(await browser.$('nav[aria-label="Pipelines views"]').$("button=Runs").getAttribute("aria-current"), "page");
  const selectedRun = execution.$(".//label[starts-with(normalize-space(.), 'Saved workflow run')]/select");
  assert.equal(await selectedRun.getValue(), run.id);
  await textIsVisible(browser, execution, task.task.id);
  const snapshot = await invoke(browser, "inspect_workflow_execution", { projectId, runId: run.id });
  assert.deepEqual(snapshot.run.version, version);
  assert.equal(snapshot.launch.origin.kind, "agile");
  assert.equal(snapshot.launch.origin.workId, task.task.id);
  assert.equal(snapshot.launch.source.id, sourceRevision.id);
  const startTask = snapshot.outputs.find(({ current, artifact }) =>
    current && artifact.nodeInstanceId === start && artifact.port === "task");
  assert.deepEqual(startTask.artifact.value, { kind: "task", evidence: { projectId, taskId: task.task.id } });
  await fitGraph(browser, '[aria-label="Workflow execution graph"]');
  await capture?.(browser, "visual-pipeline-task-launch");
}

export async function verifyVisualPipelineBuilder({ browser, application, checkout, invoke, restart, capture, emulateDisplay }) {
  const { checkoutId, checkoutRoot, projectId } = checkout;
  const nonce = randomUUID().slice(0, 8);
  const title = `Visual pipeline ${nonce}`;
  const markdownName = `Visual pipeline ${nonce}.md`;
  const markdown = `# ${title}\n\nExact document pinned by the real launch.\n`;
  const file = join(checkoutRoot, markdownName);
  await writeFile(file, markdown, { flag: "wx" });

  await openBuilder(browser);
  const views = browser.$('[aria-label="Pipelines views"]');
  await click(views.$("button=Library"));
  const library = browser.$('[aria-label="Workflow library"]');
  const libraryViews = browser.$('[aria-label="Library views"]');
  await click(libraryViews.$("button=Schemas"));
  await library.$("button=New Schema").waitForDisplayed({ timeout });
  for (const [category, open, create] of [["Node Prefabs", "New Node Prefab", "Create Node Prefab"], ["Functions", "New Function", "Create Function"]]) {
    await click(libraryViews.$(`button=${category}`));
    await click(library.$(`button=${open}`));
    const reusable = browser.$('[role="dialog"]');
    await reusable.$(`button=${create}`).waitForDisplayed({ timeout });
    await browser.keys("Escape");
    await reusable.waitForDisplayed({ timeout, reverse: true });
  }
  assert.equal(await library.$("[data-pipeline-node]").isExisting(), false);
  assert.equal(await library.$("summary=Import or export workflow packages").isExisting(), false);
  await setWindow(browser, invoke, 640, 440);
  await click(library.$('button[aria-label="View Clarify Requirements"]'));
  const preview = browser.$('[aria-label="Artifact preview"]');
  await preview.$('input[aria-label="Function name"]').waitForDisplayed({ timeout });
  const back = preview.$("button=Voltar");
  await back.waitForDisplayed({ timeout });
  await browser.waitUntil(() => browser.execute(() => document.activeElement?.matches("[data-workflow-back]")), { timeout });
  await capture?.(browser, "review-library-function-compact");
  await browser.keys("Enter");
  await library.$('button[aria-label="View Clarify Requirements"]').waitForDisplayed({ timeout });
  await setWindow(browser, invoke, 1440, 900);
  await click(libraryViews.$("button=Roles"));
  await browser.$('[aria-label="Role library"]').waitForDisplayed({ timeout });
  await click(libraryViews.$("button=Pipeline"));
  await library.waitForDisplayed({ timeout });
  const discardName = "Discard this fixture pipeline";
  const discarded = await invoke(browser, "create_workflow_package", { projectId, conversationId: null, package: {
    name: discardName, members: [{ key: "pipeline", name: discardName, body: { kind: "pipeline",
      graph: { name: discardName, nodes: [{ id: "start", kind: "start", configuration: { kind: "none" } }], wires: [] },
    } }],
  } });
  await click(libraryViews.$("button=Functions"));
  await click(libraryViews.$("button=Pipeline"));
  await click(library.$(`button[aria-label=${JSON.stringify("Open draft of " + discardName)}]`));
  const draftName = browser.$(`${builder} input[aria-label="Pipeline name"]`);
  await replaceText(browser, draftName, "Unsaved pipeline name");
  await click(libraryViews.$("button=Functions"));
  const unsaved = browser.$('[role="dialog"]');
  await unsaved.$("h2=Unsaved changes").waitForDisplayed({ timeout });
  await browser.waitUntil(() => browser.execute(() => document.activeElement?.textContent.trim() === "Keep editing"), { timeout });
  await browser.keys("Enter");
  assert.equal(await draftName.getValue(), "Unsaved pipeline name");
  await click(libraryViews.$("button=Functions"));
  await click(unsaved.$("button=Discard changes and continue"));
  await click(libraryViews.$("button=Pipeline"));
  const discardSelector = `button[aria-label=${JSON.stringify("Actions for " + discardName)}]`;
  const discardMenu = library.$(discardSelector);
  await discardMenu.waitForDisplayed({ timeout });
  await capture?.(browser, "review-library-draft-actions");
  await browser.execute((selector) => document.querySelector(selector).focus(), discardSelector);
  await browser.keys([Key.Shift, Key.F10]);
  await click(browser.$("//div[@role='menuitem' and normalize-space(.)='Delete draft']"));
  await browser.waitUntil(() => browser.execute(() => document.activeElement?.textContent.trim() === "Cancel"), { timeout });
  await click(browser.$('[role="dialog"]').$("button=Delete draft"));
  await discardMenu.waitForExist({ timeout, reverse: true });
  await assert.rejects(invoke(browser, "inspect_workflow_package", { projectId, draftId: discarded.id }), /unavailable/i);
  await click(views.$("button=Maintenance"));
  const maintenance = browser.$('[aria-label="Workflow maintenance"]');
  await maintenance.$("h3=Proposed changes").waitForDisplayed({ timeout });
  await maintenance.$("summary=Configure maintenance workflow").waitForDisplayed({ timeout });
  await maintenance.$("h3=CLI plugins").waitForDisplayed({ timeout });
  assert.equal(await maintenance.$("details.pipeline-maintenance-rules").getAttribute("open"), null);
  await capture?.(browser, "real-pipeline-maintenance");
  await click(views.$("button=Compare models"));
  const comparison = browser.$('[aria-label="Model comparisons"]');
  await comparison.$("h3=Request and context").waitForDisplayed({ timeout });
  await comparison.$("h3=Candidates").waitForDisplayed({ timeout });
  await click(views.$("button=Builder"));
  const landing = browser.$('[aria-label="Pipeline builder"]');
  await landing.$("h2=Build a pipeline").waitForDisplayed({ timeout });
  await capture?.(browser, "real-pipeline-builder-home");
  await click(landing.$("button=Create or edit with AI"));
  const authoring = browser.$('[role="dialog"]');
  await authoring.$("h2=Create or edit with AI").waitForDisplayed({ timeout });
  for (const output of ["pipeline", "node_prefab", "function", "role", "schema", "package"]) {
    assert.equal(await authoring.$(`select option[value="${output}"]`).isExisting(), true);
  }
  await select(authoring, "Output", "pipeline");
  assert.equal(await authoring.$(".//label[starts-with(normalize-space(.), 'Source conversation')]").isExisting(), false);
  assert.equal(await authoring.$("button=Start authoring request").isEnabled(), false);
  await capture?.(browser, "real-pipeline-builder-ai");
  await browser.keys("Escape");
  await authoring.waitForDisplayed({ timeout, reverse: true });
  await click(browser.$("button=New pipeline"));
  const dialog = browser.$('[role="dialog"]');
  const name = dialog.$(".//input[@aria-label='Pipeline name'] | .//label[normalize-space(.)='Pipeline name']/input");
  await name.waitForDisplayed({ timeout });
  await name.setValue(title);
  await click(dialog.$("button=Create pipeline"));

  const editor = browser.$(builder);
  await editor.waitForDisplayed({ timeout });
  const staged = await invoke(browser, "list_workflow_packages", { projectId });
  const draft = staged.find(({ name }) => name === title);
  assert.ok(draft, "The named builder action did not create a project draft");
  const draftScope = { projectId, draftId: draft.id };
  const canvasActions = `${canvas} [aria-label="Workflow canvas actions"]`;
  assert.ok(await browser.execute((selector) => {
    const surface = document.querySelector(selector);
    surface?.focus();
    return document.activeElement === surface;
  }, canvasActions));
  await browser.keys([Key.Shift, Key.F10]);
  const insertNode = browser.$('[role="dialog"]');
  await insertNode.$("button=AI Node Agent").waitForDisplayed({ timeout });
  const choices = await browser.execute(() =>
    [...document.querySelectorAll(".workflow-node-picker-list button")].map((choice) => choice.textContent.trim()));
  assert.deepEqual(choices, [
    "AI Node Agent", "Parallel / Fork", "Join", "Wait", "Loop", "Human in the Loop",
  ]);
  await browser.keys("Escape");
  assert.equal(await editor.$('[aria-label="Add a reusable node"]').isExisting(), false);
  const seeded = await invoke(browser, "inspect_workflow_package", draftScope);
  const seedGraph = seeded.package.members[0].body.graph;
  assert.deepEqual(seedGraph.nodes.map(({ kind }) => kind), ["typed_start", "end"]);
  assert.deepEqual(seedGraph.wires, [{ from: { node: "start", port: "start" }, to: { node: "end", port: "ready" } }]);
  seedGraph.nodes.splice(1, 0, { id: wait, kind: "wait", configuration: {
    kind: "wait", until: { kind: "delay", seconds: 60 },
  } });
  seedGraph.wires = [];
  const initial = await invoke(browser, "save_workflow_package", {
    ...draftScope, expectedRevision: seeded.revision, package: seeded.package,
  });
  await browser.refresh();
  await openBuilder(browser);
  await click(browser.$(`.//li[.//strong[normalize-space(.)='${title}']]//button[normalize-space(.)='Open draft']`));
  await browser.$(`${canvas} ${waitNode}`).waitForDisplayed({ timeout });
  await browser.$(canvas).scrollIntoView({ block: "end" });
  await click(browser.$(`${canvas} .react-flow__controls-fitview`));

  const initialPosition = await nodePosition(browser);
  const position = await locate(browser, `${canvas} ${waitNode} header`);
  const destination = await browser.execute((selector, origin) => {
    const graph = window.document.querySelector(selector);
    const rect = graph.getBoundingClientRect();
    const transform = new DOMMatrixReadOnly(getComputedStyle(graph.querySelector(".react-flow__viewport")).transform);
    return {
      x: Math.round(Math.min(rect.right - 25, innerWidth - 25, origin.x + 305 * transform.a)),
      y: Math.round(Math.max(rect.top + 30, origin.y - 172 * transform.a)),
    };
  }, canvas, position);
  await drag(browser, position, destination);
  await browser.waitUntil(async () => {
    const current = await nodePosition(browser);
    return current && Math.abs(current.x - initialPosition.x) + Math.abs(current.y - initialPosition.y) > 8;
  }, { timeout, timeoutMsg: "Dragging Wait did not move its canvas position" });
  const positioned = await nodePosition(browser);
  await click(browser.$(`${canvas} .react-flow__controls-fitview`));
  // Leave room for the pointer outside the canvas's edge-panning zones during the connection.
  await click(browser.$(`${canvas} .react-flow__controls-zoomout`));

  await drag(browser, await locate(browser, handle(start, "start")), await locate(browser, handle(wait, "ready")));
  await browser.$(edge(waitReady)).waitForDisplayed({ timeout });
  await connectByKeyboard(browser, waitInput);
  await browser.$(edge(waitInput)).waitForDisplayed({ timeout });

  const incompatible = makeWire(start, "document", end, "ready");
  await connectByKeyboard(browser, incompatible);
  const connectionError = editor.$('[role="alert"]');
  await textIsVisible(browser, connectionError, "start.document (markdown) cannot connect to end.ready (Control)");
  assert.equal(await browser.$(edge(incompatible)).isExisting(), false);
  await connectionError.scrollIntoView({ block: "center" });
  await capture?.(browser, "visual-pipeline-incompatible-ports");
  await connectByKeyboard(browser, endReady);
  await browser.$(edge(endReady)).waitForDisplayed({ timeout });
  await connectByKeyboard(browser, endResult);
  await browser.$(edge(endResult)).waitForDisplayed({ timeout });
  await select(editor.$('[aria-label="Node configuration"]'), "Inspect node", wait);
  await replaceText(browser, editor.$(".//label[normalize-space(.)='Delay (seconds)']/input"), "1");

  await fitGraph(browser, canvas);
  await capture?.(browser, "visual-pipeline-edited");

  await click(browser.$("button=Save draft revision"));
  let saved;
  await browser.waitUntil(async () => {
    saved = await invoke(browser, "inspect_workflow_package", draftScope);
    return Boolean(saved && saved.revision !== initial.revision);
  }, { timeout, timeoutMsg: "Saving did not produce a new immutable pipeline draft revision" });
  const graph = saved.package.members.find(({ key }) => key === "pipeline").body.graph;
  assert.equal(graph.name, title);
  assert.deepEqual(graph.nodes.map(({ id, kind }) => ({ id, kind })), [
    { id: start, kind: "typed_start" }, { id: wait, kind: "wait" }, { id: end, kind: "end" },
  ]);
  assert.deepEqual(graph.nodes.find(({ id }) => id === wait).configuration.until, { kind: "delay", seconds: 1 });
  assertWires(graph.wires);
  closePosition(saved.layouts.pipeline[wait], positioned);

  browser = await restart(browser);
  await openBuilder(browser);
  await click(browser.$(`.//li[.//strong[normalize-space(.)='${title}']]//button[normalize-space(.)='Open draft']`));
  await browser.$(builder).waitForDisplayed({ timeout });
  for (const wire of wires) await browser.$(edge(wire)).waitForDisplayed({ timeout });
  closePosition(await nodePosition(browser), saved.layouts.pipeline[wait]);
  assert.equal((await invoke(browser, "inspect_workflow_package", draftScope)).revision, saved.revision);
  await capture?.(browser, "visual-pipeline-restored");
  await verifyResponsiveView(browser, invoke, capture, emulateDisplay, {
    name: "visual-pipeline-builder", canvas, inspector: '[aria-label="Node configuration"]', nodeLabel: "Inspect node",
  });

  const listRuns = () => invoke(browser, "list_repository_deliveries", { projectId });
  const previousRuns = new Set((await listRuns()).map(({ id }) => id));
  const listJobs = async () => (await invoke(browser, "project_job_queue", { projectId })).jobs;
  const previousJobs = await listJobs();
  await reviewSavedRevision(browser, saved);
  assert.deepEqual(new Set((await listRuns()).map(({ id }) => id)), previousRuns);
  assert.deepEqual(await listJobs(), previousJobs);
  assert.equal((await invoke(browser, "inspect_workflow_package", draftScope)).publication, null);
  await capture?.(browser, "visual-pipeline-reviewed");
  await click(browser.$('button[aria-label="Close review workflow package"]'));

  await click(browser.$("button=Review publication"));
  const publish = browser.$('[role="dialog"]');
  await textIsVisible(browser, publish, saved.revision);
  await click(publish.$("button=Confirm publication"));
  const published = browser.$('[aria-label="Publication receipt"]');
  await textIsVisible(browser, published, saved.revision);
  const { publication } = await invoke(browser, "inspect_workflow_package", draftScope);
  assert.equal(publication.reviewedRevision, saved.revision);
  const { id, revision } = publication.members.find(({ key }) => key === "pipeline").artifact.version;
  const version = { id, revision };

  await click(published.$("button=Run published workflow"));
  await select(published, "Published workflow revision", JSON.stringify([id, revision]));
  const startInputs = published.$('[aria-label="Start inputs"]');
  await select(startInputs, "Checkout", checkoutId);
  await click(startInputs.$(".//label[contains(normalize-space(.),'Include a Markdown revision')]/input"));
  const source = startInputs.$('[aria-label="Repository Markdown source"]');
  await chooseMarkdown(application, source.$("button=Select Markdown file"), file);
  let sourceRevision;
  await browser.waitUntil(async () => {
    sourceRevision = (await invoke(browser, "list_repository_markdown_revisions", { checkoutId }))
      .find(({ relativePath }) => relativePath === markdownName);
    return Boolean(sourceRevision);
  }, { timeout });
  assert.equal(sourceRevision.sha256, createHash("sha256").update(markdown).digest("hex"));
  await select(source, "Saved source revision", sourceRevision.id);
  await click(source.$("button=Start workflow"));

  let run;
  await browser.waitUntil(async () => {
    run = (await listRuns()).find(({ id: runId, version: pin }) =>
      !previousRuns.has(runId) && pin.id === id && pin.revision === revision);
    return Boolean(run);
  }, { timeout, timeoutMsg: "The UI did not start the exact published workflow revision" });
  const execution = browser.$('[aria-label="Pinned workflow execution"]');
  await textIsVisible(browser, execution, revision);
  const nodeEvidence = execution.$('[aria-label="Execution node evidence"]');
  await select(nodeEvidence, "Inspect execution node", end);
  await textIsVisible(browser, nodeEvidence, "Selected ports · completed");
  const snapshot = await invoke(browser, "inspect_workflow_execution", { projectId, runId: run.id });
  assert.deepEqual(snapshot.run.version, version);
  closePosition(snapshot.layout[wait], saved.layouts.pipeline[wait]);
  const executionCanvas = '[aria-label="Workflow execution graph"]';
  closePosition(await nodePosition(browser, executionCanvas), saved.layouts.pipeline[wait]);
  assert.equal(snapshot.launch.checkoutId, checkoutId);
  assert.equal(snapshot.launch.source.id, sourceRevision.id);
  assert.equal(snapshot.launch.source.sha256, sourceRevision.sha256);
  assert.equal(snapshot.launch.source.checkoutId, checkoutId);
  assert.equal(snapshot.launch.source.relativePath, markdownName);
  assert.equal(snapshot.launch.origin.kind, "editor");
  assertWires(snapshot.definition.wires);
  const startOutput = snapshot.outputs.find(({ current, artifact }) =>
    current && artifact.nodeInstanceId === start && artifact.port === "document");
  assert.equal(startOutput.artifact.value.evidence.id, sourceRevision.id);
  const inputsOutput = snapshot.outputs.find(({ current, artifact }) =>
    current && artifact.nodeInstanceId === start && artifact.port === "inputs");
  const waitOutput = snapshot.outputs.find(({ current, artifact }) =>
    current && artifact.nodeInstanceId === wait && artifact.port === "result");
  const endOutput = snapshot.outputs.find(({ current, artifact }) =>
    current && artifact.nodeInstanceId === end && artifact.port === "result");
  assert.deepEqual(waitOutput.artifact.value, inputsOutput.artifact.value);
  assert.deepEqual(endOutput.artifact.value, waitOutput.artifact.value);
  assert.equal(waitOutput.inputReceipts[0].inputs.input, inputsOutput.artifact.id);
  assert.equal(endOutput.inputReceipts[0].inputs.result, waitOutput.artifact.id);
  await select(nodeEvidence, "Inspect execution node", wait);
  await textIsVisible(browser, nodeEvidence, "Selected ports · completed");
  assert.equal(snapshot.jobs.length, 0);
  await fitGraph(browser, executionCanvas);
  await capture?.(browser, "visual-pipeline-real-run");
  await verifyResponsiveView(browser, invoke, capture, emulateDisplay, {
    name: "visual-pipeline-runs", canvas: executionCanvas, inspector: '[aria-label="Execution node evidence"]', nodeLabel: "Inspect execution node",
  });
  await launchFromTask({ browser, invoke, capture, checkout, title: `Task for ${title}`, version, sourceRevision });

  const chatTitle = `Conversation template for ${title}`;
  const chatDraft = await invoke(browser, "create_workflow_package", {
    projectId, package: { name: chatTitle, members: [
      { key: "chat-turn", name: "Conversation turn", body: { kind: "schema", fields: [
        { name: "message", type: "string", required: true },
        { name: "reply", type: "string", required: true },
      ] } },
      { key: "pipeline", name: chatTitle, references: {
        "start:schema": { source: "draft", key: "chat-turn" },
        "end:schema": { source: "draft", key: "chat-turn" },
      }, body: { kind: "pipeline", graph: { name: chatTitle, nodes: [
        { id: "start", kind: "typed_start", configuration: { kind: "typed_start", conversation: true, fields: [] } },
        { id: "feedback", kind: "human", configuration: { kind: "human", interaction: "feedback", message: "Add a note about this revision", fields: [
          { id: "note", label: "Review note", required: true, input: { kind: "text" } },
        ] } },
        { id: "end", kind: "end", configuration: { kind: "end" } },
      ], wires: [
        makeWire(start, "start", "feedback", "ready"),
        makeWire("feedback", "answered", end, "ready"),
        makeWire(start, "inputs", end, "result"),
      ] } } },
    ] },
  });
  const publishedChat = await invoke(browser, "publish_reviewed_workflow_package", {
    projectId, draftId: chatDraft.id, reviewedRevision: chatDraft.revision,
  });
  const chatArtifact = publishedChat.publication.members.find(({ key }) => key === "pipeline").artifact.version;
  const chatVersion = { id: chatArtifact.id, revision: chatArtifact.revision };
  await click(browser.$('button[aria-label="Conversations"]'));
  await click(browser.$('button[aria-label="New conversation"]'));
  const picker = browser.$('.conversation-pipeline-dialog[role="dialog"]');
  await click(picker.$(`.//label[.//strong[normalize-space(.)='${chatTitle}']]`));
  await click(picker.$("button=Create conversation"));
  let conversation;
  await browser.waitUntil(async () => {
    conversation = (await invoke(browser, "list_conversations", { checkoutId }))
      .find(({ pipeline }) => pipeline?.kind === "published" && pipeline.workflow.id === chatVersion.id);
    return Boolean(conversation);
  }, { timeout, timeoutMsg: "The selected published conversation was not persisted" });
  assert.deepEqual(conversation.pipeline.workflow, chatVersion);
  const chat = browser.$(`section[aria-label=${JSON.stringify(conversation.title)}]`);
  const composer = chat.$('textarea[id^="draft-"]');
  const turns = () => invoke(browser, "conversation_pipeline_turns", { conversationId: conversation.id });
  const reply = "Recorded the requested feedback.";
  async function finishWaitingTurn(index) {
    await click(chat.$("button=Open waiting step"));
    const execution = browser.$('[aria-label="Pinned workflow execution"]');
    const graph = execution.$('[aria-label="Workflow execution graph"]');
    const feedback = graph.$('.react-flow__node[data-id="feedback"]');
    const end = graph.$('.react-flow__node[data-id="end"]');
    await feedback.$('[data-status="waiting"]').waitForDisplayed({ timeout });
    const form = execution.$('[aria-label="Workflow feedback"]');
    const note = `Note for turn ${index + 1}`;
    await replaceText(browser, form.$('input[type="text"]'), note);
    await click(form.$("button=Send feedback"));
    await feedback.$('[data-status="completed"]').waitForDisplayed({ timeout });
    await end.$('[data-status="completed"]').waitForDisplayed({ timeout });
    const inspector = execution.$('[aria-label="Execution node evidence"]');
    const response = inspector.$('[aria-label="response output"]');
    await click(response.$("summary=Open output evidence"));
    await textIsVisible(browser, response, note);
    await select(inspector, "Inspect execution node", "end");
    const result = inspector.$('[aria-label="result output"]');
    await click(result.$("summary=Open output evidence"));
    await textIsVisible(browser, result, reply);
    await click(execution.$("button=Open source conversation"));
    await browser.waitUntil(async () => (await turns())[index]?.state === "succeeded", { timeout });
    await textIsVisible(browser, chat, reply);
  }
  await replaceText(browser, composer, "Read this saved revision once.");
  await click(chat.$("button=Choose inputs"));
  const chatInputs = browser.$('.conversation-pipeline-dialog[role="dialog"]');
  await select(chatInputs, "Saved Markdown revision", sourceRevision.id);
  await replaceText(browser, chatInputs.$(".//label[normalize-space(.)='reply']/input"), reply);
  await click(chatInputs.$("button=Send with these inputs"));
  await finishWaitingTurn(0);
  await replaceText(browser, composer, "Use the same revision again.");
  await click(chat.$("button=Run pipeline"));
  await finishWaitingTurn(1);
  const attempts = await turns();
  assert.deepEqual(attempts.map(({ inputs }) => inputs.documentRevisionId), [sourceRevision.id, sourceRevision.id]);
  assert.deepEqual(attempts.map(({ inputs }) => inputs.values.reply), [reply, reply]);
  assert.notEqual(attempts[0].runId, attempts[1].runId);
  for (const { runId } of attempts) {
    const attempt = await invoke(browser, "inspect_workflow_execution", { projectId, runId });
    assert.deepEqual(attempt.run.version, chatVersion);
  }
  await capture?.(browser, "real-conversation-pipeline-turns");

  const configured = (await invoke(browser, "cli_executables")).find(({ provider }) => provider === "codex");
  assert.ok(configured);
  await invoke(browser, "set_cli_executable", { provider: "codex", executable: application });
  try {
    const model = (await invoke(browser, "conversation_models", { provider: "codex" }))
      .find(({ option }) => option.selectable && option.selection.modelId === "desktop-fixture");
    assert.ok(model);
    const title = "Continue into a pipeline";
    const direct = await invoke(browser, "create_conversation", { checkoutId, title });
    const directScope = { conversationId: direct.id };
    const previous = "Complete desktop slot probe";
    await invoke(browser, "save_conversation_draft", { ...directScope, text: previous });
    await invoke(browser, "start_conversation_turn", { ...directScope, selection: model.option.selection, mode: "read_only", text: previous });
    await browser.waitUntil(async () => (await invoke(browser, "latest_conversation_run", directScope))?.status === "succeeded", { timeout });
    let directTurn;
    await browser.waitUntil(async () => {
      [directTurn] = await invoke(browser, "conversation_pipeline_turns", directScope);
      return directTurn?.state === "succeeded";
    }, { timeout, timeoutMsg: "The direct conversation did not reach End" });
    const directRun = await invoke(browser, "inspect_workflow_execution", { projectId, runId: directTurn.runId });
    assert.deepEqual(directRun.definition.nodes.map(({ instance }) => instance.kind), ["typed_start", "agent", "end"]);
    assert.ok(directRun.control.some(({ nodeId, state }) => nodeId === "end" && state === "completed"));
    await browser.refresh();
    await click(browser.$('button[aria-label="Conversations"]'));
    await click(browser.$(`section[aria-label="Conversation library"] button[aria-label=${JSON.stringify(title)}]`));
    const panel = browser.$(`section[aria-label=${JSON.stringify(title)}]`);
    await click(panel.$(".model-picker-trigger"));
    await click(browser.$(`.model-picker-menu [role="menuitemradio"][aria-label=${JSON.stringify(model.option.name)}]`));
    const mode = panel.$('select[aria-label="Conversation mode"]');
    await mode.selectByAttribute("value", "plan");
    const draft = "Keep this as the first pipeline draft";
    await replaceText(browser, panel.$('textarea[id^="draft-"]'), draft);
    await click(panel.$("button=Send"));
    const continuationDialog = browser.$('.conv-dialog[role="dialog"]');
    await textIsVisible(browser, continuationDialog, "Choose how to continue");
    await click(continuationDialog.$('.//label[contains(normalize-space(.), "Start a fresh conversation")]'));
    assert.equal(await continuationDialog.$("button=Continue and send").isEnabled(), false);
    await click(continuationDialog.$(`.//label[.//code[@title='${chatVersion.revision}']]`));
    await capture?.(browser, "real-continuation-pipeline-picker");
    await click(continuationDialog.$("button=Open draft"));
    const fresh = browser.$('section[aria-label="New conversation"] textarea[id^="draft-"]');
    await browser.waitUntil(async () => await fresh.isDisplayed() && (await fresh.getValue()) === draft, { timeout });
    const target = (await invoke(browser, "list_conversations", { checkoutId })).find((item) => item.title === "New conversation");
    assert.deepEqual(target?.pipeline.workflow, chatVersion);
    assert.equal(await invoke(browser, "conversation_draft", directScope), "");
    assert.deepEqual(await invoke(browser, "conversation_pipeline_turns", { conversationId: target.id }), []);
  } finally {
    await invoke(browser, "set_cli_executable", { provider: "codex", executable: configured.executable });
  }
  return browser;
}

export async function verifyVisualPromptBuilder({ browser, checkout, invoke, capture }) {
  const { projectId, checkoutId } = checkout;
  const title = "Typed AI pipeline " + randomUUID().slice(0, 8);
  const models = (await invoke(browser, "conversation_models", { provider: "codex" }))
    .filter(({ option }) => option.selection.modelId === "desktop-fixture" && option.selectable);
  assert.equal(models.length, 1, "The test needs the fixture Role model");
  const reference = (key) => ({ source: "draft", key });
  const schema = (key, name, fields) => ({ key, name, body: { kind: "schema", fields } });
  const agent = (key, name, inputSchema, outputSchema) => ({
    key, name,
    body: { kind: "prompt", prompt: { text: "Draft instructions.", executor: "agent", schemaContract: true } },
    references: {
      role: reference("fixture-role"), input_schema: reference(inputSchema), output_schema: reference(outputSchema),
    },
  });
  const packageDocument = {
    name: title,
    members: [
      { key: "pipeline", name: title, references: {
        "start:schema": reference("request"), "end:schema": reference("reply"),
        prompt: reference("first-agent"), "prompt-2": reference("second-agent"),
      }, body: { kind: "pipeline", graph: { name: title, nodes: [
        { id: start, kind: "typed_start", configuration: { kind: "typed_start", fields: [] } },
        { id: "prompt", kind: "subflow", configuration: { kind: "none" } },
        { id: "prompt-2", kind: "subflow", configuration: { kind: "none" } },
        { id: end, kind: "end", configuration: { kind: "end" } },
      ], wires: [] } } },
      schema("request", "Desktop request", []),
      schema("summary", "Review summary", [{ name: "summary", type: "string", required: true }]),
      schema("reply", "Final reply", [{ name: "reply", type: "string", required: true }]),
      { key: "fixture-role", name: "Desktop analyst", body: { kind: "role", template: {
        name: "Desktop analyst", role: "pipeline", model: models[0].option.selection, effort: null,
        instructions: "Read the supplied test inputs.", toolScope: "read_only",
        completionContract: "Return the requested result.", reviewPolicy: "independent",
      } } },
      agent("first-agent", "First agent", "request", "summary"),
      agent("second-agent", "Second agent", "summary", "reply"),
    ],
  };
  const created = await invoke(browser, "create_workflow_package", {
    projectId, conversationId: null, package: packageDocument,
  });
  const draftScope = { projectId, draftId: created.id };
  await setWindow(browser, invoke, 1440, 900);
  await openBuilder(browser);
  await click(browser.$('nav[aria-label="Pipelines views"]').$("button=Library"));
  await click(browser.$(`[aria-label="Workflow library"] button[aria-label=${JSON.stringify("Open draft of " + title)}]`));
  const editor = browser.$(builder);
  const inspector = editor.$('[aria-label="Node configuration"]');
  const promptSettings = inspector.$('[aria-label="AI Node Agent settings"]');
  const promptText = promptSettings.$(".//label[starts-with(normalize-space(.), 'Prompt')]/textarea");
  const evaluator = "Evaluate the desktop workflow input.";
  const summarizer = "Summarize the desktop workflow result: {{input.summary}}";
  await select(inspector, "Inspect node", "prompt");
  await replaceText(browser, promptSettings.$(".//label[normalize-space(.)='Card title']/input"), "Evaluate changes");
  await replaceText(browser, promptText, evaluator);
  await select(inspector, "Inspect node", "prompt-2");
  await replaceText(browser, promptSettings.$(".//label[normalize-space(.)='Card title']/input"), "Read summary");
  await replaceText(browser, promptText, summarizer);

  const first = makeWire(start, "start", "prompt", "ready");
  const input = makeWire(start, "inputs", "prompt", "input");
  const next = makeWire("prompt", "completed", "prompt-2", "ready");
  const data = makeWire("prompt", "result", "prompt-2", "input");
  const finish = makeWire("prompt-2", "completed", end, "ready");
  const result = makeWire("prompt-2", "result", end, "result");
  await connectByKeyboard(browser, first);
  await connectByKeyboard(browser, input);
  await connectByKeyboard(browser, next);
  const hints = editor.$('[aria-label="Suggested data connections"]');
  await hints.waitForDisplayed({ timeout });
  assert.equal(await browser.$(edge(data)).isExisting(), false, "A suggested data wire needs confirmation");
  await click(hints.$("button*=result"));
  await connectByKeyboard(browser, finish);
  await connectByKeyboard(browser, result);
  const expectedWires = [first, input, next, data, finish, result].map(wireLabel).sort();
  for (const wire of [first, input, next, data, finish, result]) await browser.$(edge(wire)).waitForDisplayed({ timeout });
  await capture?.(browser, "visual-prompt-contract");

  const prefabName = "Reusable desktop evaluator";
  await select(inspector, "Inspect node", "prompt");
  await click(promptSettings.$("button=Save as Node Prefab"));
  const prefabDialog = browser.$('[role="dialog"]');
  await replaceText(browser, prefabDialog.$(".//label[normalize-space(.)='Prefab name']/input"), prefabName);
  await click(prefabDialog.$("button=Add to draft artifacts"));
  await click(browser.$("button=Save draft revision"));
  let saved;
  await browser.waitUntil(async () => {
    saved = await invoke(browser, "inspect_workflow_package", draftScope);
    return saved.revision !== created.revision;
  }, { timeout });
  const authored = saved.package.members.filter(({ body }) => body.kind === "prompt");
  assert.deepEqual(authored.map(({ name }) => name).sort(), ["Evaluate changes", "Read summary", prefabName].sort());
  const firstAgent = authored.find(({ key }) => key === "first-agent");
  const secondAgent = authored.find(({ key }) => key === "second-agent");
  assert.equal(firstAgent.body.prompt.text, evaluator);
  assert.equal(secondAgent.body.prompt.text, summarizer);
  assert.equal(authored.find(({ name }) => name === prefabName).body.prompt.nodePrefab, true);
  assert.deepEqual(saved.package.members[0].body.graph.wires.map(wireLabel).sort(), expectedWires);
  const previousQueue = (await invoke(browser, "project_job_queue", { projectId })).jobs;

  const review = await reviewSavedRevision(browser, saved);
  const artifacts = review.$(".//label[starts-with(normalize-space(.), 'Review artifact')]/select");
  await artifacts.selectByAttribute("value", firstAgent.key);
  await click(review.$("summary=Dependencies"));
  await textIsVisible(browser, review, "Draft fixture-role");
  assert.deepEqual((await invoke(browser, "project_job_queue", { projectId })).jobs, previousQueue);
  await capture?.(browser, "visual-prompt-reviewed");
  await click(browser.$('button[aria-label="Close review workflow package"]'));

  await click(browser.$("button=Review publication"));
  const confirmation = browser.$('[role="dialog"]');
  await click(confirmation.$("button=Confirm publication"));
  const receipt = browser.$('[aria-label="Publication receipt"]');
  await textIsVisible(browser, receipt, saved.revision);
  const { publication } = await invoke(browser, "inspect_workflow_package", draftScope);
  const pipeline = publication.members.find(({ key }) => key === "pipeline").artifact.version;
  await click(receipt.$("button=Run published workflow"));
  const inputs = receipt.$('[aria-label="Start inputs"]');
  await select(inputs, "Checkout", checkoutId);
  await click(inputs.$(".//label[contains(normalize-space(.),'Dispatch ready workflow stages automatically')]/input"));
  await click(inputs.$("button=Start workflow"));
  const execution = browser.$('[aria-label="Pinned workflow execution"]');
  await execution.waitForDisplayed({ timeout });
  const nodeEvidence = execution.$('[aria-label="Execution node evidence"]');
  await select(nodeEvidence, "Inspect execution node", "prompt-2");
  await textIsVisible(browser, nodeEvidence.$('[aria-label="Result on result"]'), "The desktop fixture read the summary.");
  await click(nodeEvidence.$("button=Open internal graph"));
  await click(execution.$("button=Voltar"));
  assert.equal(await nodeEvidence.$(".//label[starts-with(normalize-space(.), 'Inspect execution node')]/select").getValue(), "prompt-2");
  await select(nodeEvidence, "Inspect execution node", end);
  await textIsVisible(browser, nodeEvidence, "Selected ports · completed");
  const runs = await invoke(browser, "list_repository_deliveries", { projectId });
  const launched = runs.find(({ version }) => version.id === pipeline.id && version.revision === pipeline.revision);
  assert.ok(launched, "The published AI pipeline must be launched");
  const snapshot = await invoke(browser, "inspect_workflow_execution", { projectId, runId: launched.id });
  assert.equal(snapshot.launch.automatic, true);
  assert.deepEqual(snapshot.definition.subflows.map(({ path }) => path).sort(), ["prompt", "prompt-2"]);
  const recordedResult = snapshot.outputs.find(({ current, artifact }) =>
    current && artifact.nodeInstanceId === end && artifact.port === "result");
  assert.deepEqual(recordedResult?.artifact.value.evidence, { reply: "The desktop fixture read the summary." });
  assert.equal(snapshot.jobs.filter(({ nodeId }) => nodeId.endsWith("::execute")).length, 2);
  await capture?.(browser, "visual-prompt-run");

  const prefabVersion = publication.members.find(({ artifact }) => artifact.name === prefabName).artifact.version;
  const reuseTitle = title + " reuse";
  await openBuilder(browser);
  await click(browser.$("button=New pipeline"));
  const create = browser.$('[role="dialog"]');
  const newTitle = create.$(".//input[@aria-label='Pipeline name'] | .//label[normalize-space(.)='Pipeline name']/input");
  await newTitle.waitForDisplayed({ timeout });
  await newTitle.setValue(reuseTitle);
  await click(create.$("button=Create pipeline"));
  const reuseEditor = browser.$(builder);
  await reuseEditor.waitForDisplayed({ timeout });
  const fresh = (await invoke(browser, "list_workflow_packages", { projectId })).find(({ name }) => name === reuseTitle);
  assert.ok(fresh, "The reuse case needs a separate pipeline draft");
  await click(reuseEditor.$("button=Add node"));
  const addNode = browser.$('[role="dialog"]');
  await click(addNode.$(`.//button[starts-with(normalize-space(.), '${prefabName}')]`));
  const reuseInspector = reuseEditor.$('[aria-label="Node configuration"]');
  const pinnedPrompt = reuseInspector.$('[aria-label="AI Node Agent settings"] textarea');
  await pinnedPrompt.waitForDisplayed({ timeout });
  assert.equal(await pinnedPrompt.getValue(), evaluator);
  await textIsVisible(browser, reuseInspector, "Inherited prompt");
  await click(reuseInspector.$("button=Open pinned Prefab"));
  const prefab = browser.$('[aria-label="Artifact preview"]');
  await prefab.$('[aria-label="AI Node Agent settings"] textarea').waitForDisplayed({ timeout });
  await click(prefab.$("button=Voltar"));
  assert.equal(await reuseInspector.$('[aria-label="AI Node Agent settings"] textarea').getValue(), evaluator);
  await click(browser.$("button=Save draft revision"));
  let reused;
  await browser.waitUntil(async () => {
    reused = await invoke(browser, "inspect_workflow_package", { projectId, draftId: fresh.id });
    return reused.revision !== fresh.revision;
  }, { timeout });
  assert.equal(reused.package.members.length, 1, "Reusing a Node Prefab keeps its own immutable artifact");
  assert.deepEqual(reused.package.members[0].references.subflow, { source: "published", version: prefabVersion });
  await capture?.(browser, "visual-prompt-reused");
  await setWindow(browser, invoke, 640, 440);
  const showInspector = reuseEditor.$("button=Show inspector");
  if (await showInspector.isDisplayed()) await click(showInspector);
  await click(browser.$('button[aria-label="Create Input Schema"]'));
  const schemaDialog = browser.$('.workflow-builder-dialog[role="dialog"]');
  await schemaDialog.$("h2=Create Input Schema").waitForDisplayed({ timeout });
  const schemaName = schemaDialog.$(".//label[starts-with(normalize-space(.), 'Schema name')]/input");
  await schemaName.setValue("Compact input contract");
  await click(schemaDialog.$("button=Add field"));
  await click(schemaDialog.$("button=Add field"));
  assert.equal(await browser.execute(() => {
    const dialog = document.querySelector('.workflow-builder-dialog[role="dialog"]');
    const body = dialog?.querySelector(".workflow-builder-dialog-body");
    const footer = dialog?.querySelector(".workflow-builder-dialog-footer");
    const bounds = footer?.getBoundingClientRect();
    return Boolean(body && body.scrollHeight > body.clientHeight && bounds && bounds.top >= 0 && bounds.bottom <= innerHeight);
  }), true);
  await capture?.(browser, "review-schema-dialog-compact");
  await click(schemaDialog.$("button=Cancel"));
}
