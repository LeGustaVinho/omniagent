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
const load = "load-context";
const condition = "condition";
const loadNode = `.react-flow__node[data-id="${load}"]`;
const control = { from: { node: start, port: "start" }, to: { node: load, port: "ready" } };
const documentWire = { from: { node: start, port: "document" }, to: { node: load, port: "document" } };
const conditionControl = { from: { node: load, port: "loaded" }, to: { node: condition, port: "ready" } };
const conditionValue = { from: { node: load, port: "context" }, to: { node: condition, port: "value" } };
const wires = [control, documentWire, conditionControl, conditionValue];
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
  }, `${surface} ${loadNode}`);
}

function closePosition(actual, expected) {
  assert.ok(actual && expected, "The Load context node must have a saved position");
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
      }, surface, [start, load, condition]);
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
      await select(panel, nodeLabel, load);
      await textIsVisible(browser, panel.$(".pipeline-inspector-scroll > header:first-child h2"), "Load context");
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

async function simulateSavedRevision(browser, saved, markdownName) {
  await click(browser.$("button=Review changes and simulate"));
  const review = browser.$('[aria-label="Review workflow package"]');
  const samplePath = review.$(".//label[normalize-space(.)='Sample Markdown path']/input");
  await replaceText(browser, samplePath, markdownName);
  await click(review.$("button=Simulate"));
  const trace = review.$('[aria-label="Simulation trace"]');
  await textIsVisible(browser, trace, saved.revision);
  await click(trace.$("summary=Sample inputs used"));
  await textIsVisible(browser, trace, JSON.stringify(markdownName));
  const simulationCanvas = '[aria-label="Simulated workflow graph"]';
  const graph = trace.$(simulationCanvas);
  const evidence = trace.$('[aria-label="Simulation node evidence"]');
  for (const id of [start, load, condition]) {
    await textIsVisible(browser, graph.$(`.react-flow__node[data-id="${id}"]`), "Completed");
  }
  for (const wire of wires) {
    await graph.$(`.react-flow__edge[aria-label="${wireLabel(wire)} · Selected path"]`).waitForDisplayed({ timeout });
  }
  closePosition(await nodePosition(browser, simulationCanvas), saved.layouts.pipeline[load]);
  await select(evidence, "Inspect simulated stage", load);
  await textIsVisible(browser, evidence.$('[aria-label="Stage inputs"]'), "start.document");
  await textIsVisible(browser, evidence.$('[aria-label="Stage outputs"]'), JSON.stringify(markdownName));
  await select(evidence, "Inspect simulated stage", condition);
  const branch = evidence.$('[aria-label="Stage outputs"]');
  await textIsVisible(browser, branch, "matched · control");
  assert.doesNotMatch(await branch.getText(), /unmatched/);
  await fitGraph(browser, simulationCanvas);
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
  await library.$("button=New Schema").waitForDisplayed({ timeout });
  assert.equal(await library.$("[data-pipeline-node]").isExisting(), false);
  assert.equal(await library.$("summary=Import or export workflow packages").isExisting(), false);
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
  await select(authoring, "Output", "pipeline");
  await authoring.$("option=Choose a conversation").waitForExist({ timeout });
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
  await click(editor.$("button=Add node"));
  const insertNode = browser.$('[role="dialog"]');
  await insertNode.$("button=AI Node Agent").waitForDisplayed({ timeout });
  await insertNode.$("button=Parallel / Fork").waitForDisplayed({ timeout });
  await browser.keys("Escape");
  assert.equal(await editor.$('[aria-label="Add a reusable node"]').isExisting(), false);
  const seeded = await invoke(browser, "inspect_workflow_package", draftScope);
  const seedGraph = seeded.package.members[0].body.graph;
  assert.deepEqual(seedGraph.nodes.map(({ kind }) => kind), ["typed_start", "end"]);
  assert.deepEqual(seedGraph.wires, [{ from: { node: "start", port: "start" }, to: { node: "end", port: "ready" } }]);
  seedGraph.nodes = seedGraph.nodes.filter(({ kind }) => kind !== "end");
  seedGraph.wires = [];
  seedGraph.nodes.push(
    { id: load, kind: "load_context", configuration: { kind: "none" } },
    { id: condition, kind: "condition", configuration: { kind: "condition", predicate: { op: "exists", pointer: "" } } },
  );
  const initial = await invoke(browser, "save_workflow_package", {
    ...draftScope, expectedRevision: seeded.revision, package: seeded.package,
  });
  await browser.refresh();
  await openBuilder(browser);
  await click(browser.$(`.//li[.//strong[normalize-space(.)='${title}']]//button[normalize-space(.)='Open visual draft']`));
  await browser.$(`${canvas} ${loadNode}`).waitForDisplayed({ timeout });
  await browser.$(canvas).scrollIntoView({ block: "end" });
  await click(browser.$(`${canvas} .react-flow__controls-fitview`));

  const initialPosition = await nodePosition(browser);
  const position = await locate(browser, `${canvas} ${loadNode} header`);
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
  }, { timeout, timeoutMsg: "Dragging Load context did not move its canvas position" });
  const positioned = await nodePosition(browser);
  await click(browser.$(`${canvas} .react-flow__controls-fitview`));
  // Leave room for the pointer outside the canvas's edge-panning zones during the connection.
  await click(browser.$(`${canvas} .react-flow__controls-zoomout`));

  await drag(browser, await locate(browser, handle(start, "start")), await locate(browser, handle(load, "ready")));
  await browser.$(edge(control)).waitForDisplayed({ timeout });
  await connectByKeyboard(browser, documentWire);
  await browser.$(edge(documentWire)).waitForDisplayed({ timeout });

  await select(editor.$('[aria-label="Node configuration"]'), "Inspect node", condition);
  const incompatible = { from: { node: start, port: "document" }, to: { node: condition, port: "ready" } };
  await connectByKeyboard(browser, incompatible);
  const connectionError = editor.$('[role="alert"]');
  await textIsVisible(browser, connectionError, "start.document (markdown) cannot connect to condition.ready (Control)");
  assert.equal(await browser.$(edge(incompatible)).isExisting(), false);
  await connectionError.scrollIntoView({ block: "center" });
  await capture?.(browser, "visual-pipeline-incompatible-ports");
  await connectByKeyboard(browser, conditionControl);
  await browser.$(edge(conditionControl)).waitForDisplayed({ timeout });
  await connectByKeyboard(browser, conditionValue);
  await browser.$(edge(conditionValue)).waitForDisplayed({ timeout });
  await replaceText(browser, editor.$(".//label[normalize-space(.)='Field path']/input"), "/evidence/relativePath");

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
    { id: start, kind: "typed_start" }, { id: load, kind: "load_context" }, { id: condition, kind: "condition" },
  ]);
  assert.deepEqual(graph.nodes.find(({ id }) => id === condition).configuration.predicate,
    { op: "exists", pointer: "/evidence/relativePath" });
  assertWires(graph.wires);
  closePosition(saved.layouts.pipeline[load], positioned);

  browser = await restart(browser);
  await openBuilder(browser);
  await click(browser.$(`.//li[.//strong[normalize-space(.)='${title}']]//button[normalize-space(.)='Open visual draft']`));
  await browser.$(builder).waitForDisplayed({ timeout });
  for (const wire of wires) await browser.$(edge(wire)).waitForDisplayed({ timeout });
  closePosition(await nodePosition(browser), saved.layouts.pipeline[load]);
  assert.equal((await invoke(browser, "inspect_workflow_package", draftScope)).revision, saved.revision);
  await capture?.(browser, "visual-pipeline-restored");
  await verifyResponsiveView(browser, invoke, capture, emulateDisplay, {
    name: "visual-pipeline-builder", canvas, inspector: '[aria-label="Node configuration"]', nodeLabel: "Inspect node",
  });

  const listRuns = () => invoke(browser, "list_repository_deliveries", { projectId });
  const previousRuns = new Set((await listRuns()).map(({ id }) => id));
  const listJobs = async () => (await invoke(browser, "project_job_queue", { projectId })).jobs;
  const previousJobs = await listJobs();
  await simulateSavedRevision(browser, saved, markdownName);
  assert.deepEqual(new Set((await listRuns()).map(({ id }) => id)), previousRuns);
  assert.deepEqual(await listJobs(), previousJobs);
  assert.equal((await invoke(browser, "inspect_workflow_package", draftScope)).publication, null);
  await capture?.(browser, "visual-pipeline-simulated");
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
  await select(nodeEvidence, "Inspect execution node", load);
  await textIsVisible(browser, nodeEvidence, "Completed");
  const snapshot = await invoke(browser, "inspect_workflow_execution", { projectId, runId: run.id });
  assert.deepEqual(snapshot.run.version, version);
  closePosition(snapshot.layout[load], saved.layouts.pipeline[load]);
  const executionCanvas = '[aria-label="Workflow execution graph"]';
  closePosition(await nodePosition(browser, executionCanvas), saved.layouts.pipeline[load]);
  assert.equal(snapshot.launch.checkoutId, checkoutId);
  assert.equal(snapshot.launch.source.id, sourceRevision.id);
  assert.equal(snapshot.launch.source.sha256, sourceRevision.sha256);
  assert.equal(snapshot.launch.source.checkoutId, checkoutId);
  assert.equal(snapshot.launch.source.relativePath, markdownName);
  assert.equal(snapshot.launch.origin.kind, "editor");
  assertWires(snapshot.definition.wires);
  const startOutput = snapshot.outputs.find(({ current, artifact }) =>
    current && artifact.nodeInstanceId === start && artifact.port === "document");
  const context = snapshot.outputs.find(({ current, artifact }) =>
    current && artifact.nodeInstanceId === load && artifact.port === "context");
  assert.equal(startOutput.artifact.value.evidence.id, sourceRevision.id);
  assert.deepEqual(context.artifact.value, startOutput.artifact.value);
  assert.equal(context.inputReceipts[0].inputs.document, startOutput.artifact.id);
  const branch = snapshot.control.find(({ nodeId }) => nodeId === condition);
  assert.equal(branch.state, "completed");
  assert.deepEqual(branch.selectedPorts, ["matched"]);
  await select(nodeEvidence, "Inspect execution node", condition);
  await textIsVisible(browser, nodeEvidence, "Selected ports · matched");
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
      { key: "chat-input", name: "Conversation message", body: { kind: "schema", fields: [
        { name: "message", type: "string", required: true },
      ] } },
      { key: "chat-output", name: "Conversation reply", body: { kind: "schema", fields: [
        { name: "reply", type: "string", required: true },
      ] } },
      { key: "pipeline", name: chatTitle, references: {
        "start:schema": { source: "draft", key: "chat-input" },
        "checked:schema": { source: "draft", key: "chat-output" },
        "end:schema": { source: "draft", key: "chat-output" },
      }, body: { kind: "pipeline", graph: { name: chatTitle, nodes: [
        { id: "start", kind: "typed_start", configuration: { kind: "typed_start", conversation: true, fields: [] } },
        { id: "context", kind: "load_context", configuration: { kind: "none" } },
        { id: "feedback", kind: "human", configuration: { kind: "human", interaction: "feedback", message: "Add a note about this revision", fields: [
          { id: "note", label: "Review note", required: true, input: { kind: "text" } },
        ] } },
        { id: "answer", kind: "set_fields", configuration: { kind: "set_fields", mode: "build", operations: [
          { op: "set", path: "/reply", value: { source: "literal", value: "Loaded the chosen Markdown revision." } },
        ] } },
        { id: "checked", kind: "validate_output", configuration: { kind: "validate_output", schema: { type: "object" } } },
        { id: "end", kind: "end", configuration: { kind: "end" } },
      ], wires: [
        makeWire("start", "start", "context", "ready"), makeWire("start", "document", "context", "document"),
        makeWire("context", "loaded", "feedback", "ready"), makeWire("feedback", "answered", "answer", "ready"),
        makeWire("answer", "completed", "checked", "ready"),
        makeWire("answer", "object", "checked", "source"), makeWire("checked", "completed", "end", "ready"),
        makeWire("checked", "object", "end", "result"),
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
  const reply = "Loaded the chosen Markdown revision.";
  async function finishWaitingTurn(index) {
    await click(chat.$("button=Open waiting step"));
    const execution = browser.$('[aria-label="Pinned workflow execution"]');
    const graph = execution.$('[aria-label="Workflow execution graph"]');
    const feedback = graph.$('.react-flow__node[data-id="feedback"]');
    const end = graph.$('.react-flow__node[data-id="end"]');
    await feedback.$('[data-status="waiting"]').waitForDisplayed({ timeout });
    const form = execution.$('[aria-label="Workflow input request"]');
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
  await click(chatInputs.$("button=Send with these inputs"));
  await finishWaitingTurn(0);
  await replaceText(browser, composer, "Use the same revision again.");
  await click(chat.$("button=Run pipeline"));
  await finishWaitingTurn(1);
  const attempts = await turns();
  assert.deepEqual(attempts.map(({ inputs }) => inputs.documentRevisionId), [sourceRevision.id, sourceRevision.id]);
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
    assert.deepEqual(directRun.definition.nodes.map(({ instance }) => instance.kind), ["typed_start", "conversation_agent", "end"]);
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

    const agentTitle = "Pinned agent chat";
    const agentDraft = await invoke(browser, "create_workflow_package", {
      projectId, package: { name: agentTitle, members: [{ key: "pipeline", name: agentTitle, body: {
        kind: "pipeline", graph: { name: agentTitle, nodes: [
          { id: "start", kind: "typed_start", configuration: { kind: "typed_start", fields: [] } },
          { id: "prompt", kind: "build_text", configuration: { kind: "build_text", template: "Run the revised queued instruction" } },
          { id: "reader", kind: "agent", configuration: { kind: "agent_role", role: "Reader", model: model.option.selection } },
        ], wires: [
          makeWire("start", "start", "prompt", "ready"),
          makeWire("prompt", "completed", "reader", "ready"),
          makeWire("prompt", "text", "reader", "prompt"),
        ] },
      } }] },
    });
    const publishedAgent = await invoke(browser, "publish_reviewed_workflow_package", {
      projectId, draftId: agentDraft.id, reviewedRevision: agentDraft.revision,
    });
    const pin = publishedAgent.publication.members[0].artifact.version;
    const agentChat = await invoke(browser, "create_conversation", {
      checkoutId, title: agentTitle, pipeline: { kind: "published", workflow: { id: pin.id, revision: pin.revision } },
    });
    await click(browser.$(`section[aria-label="Conversation library"] button[aria-label=${JSON.stringify(agentTitle)}]`));
    const agentPanel = browser.$(`section[aria-label=${JSON.stringify(agentTitle)}]`);
    const agentMessage = "Use this exact additional conversation context.";
    await replaceText(browser, agentPanel.$('textarea[id^="draft-"]'), agentMessage);
    await click(agentPanel.$("button=Run pipeline"));
    let agentTurn;
    await browser.waitUntil(async () => {
      [agentTurn] = await invoke(browser, "conversation_pipeline_turns", { conversationId: agentChat.id });
      return agentTurn?.state === "succeeded";
    }, { timeout, timeoutMsg: "The pinned workflow Agent did not complete the conversation turn" });
    await textIsVisible(browser, agentPanel, "The revised instruction ran.");
    const agentRun = await invoke(browser, "inspect_workflow_execution", { projectId, runId: agentTurn.runId });
    assert.deepEqual(agentRun.run.version, { id: pin.id, revision: pin.revision });
    const readerJobs = agentRun.jobs.filter(({ nodeId }) => nodeId === "reader");
    assert.equal(readerJobs.length, 1);
    const attempt = readerJobs[0].attempts[0];
    const input = await invoke(browser, "conversation_input", { conversationId: attempt.conversationId, runId: attempt.runId });
    assert.ok(input.frames.some((frame) => frame.includes(agentMessage)));
    const response = agentTurn.artifacts.find(({ kind }) => kind === "agent_response");
    assert.equal(response.nodeId, "reader");
    await click(agentPanel.$(`.//button[contains(normalize-space(.),'${response.port} · agent response')]`));
    const agentEvidence = browser.$('[aria-label="Execution node evidence"]');
    await textIsVisible(browser, agentEvidence, "Read recorded agent response");
    await capture?.(browser, "real-pipeline-agent-chat-evidence");
  } finally {
    await invoke(browser, "set_cli_executable", { provider: "codex", executable: configured.executable });
  }
  return browser;
}

export async function verifyVisualPromptBuilder({ browser, checkout, invoke, capture }) {
  const { projectId, checkoutId } = checkout;
  const title = "Prompt delivery " + randomUUID().slice(0, 8);
  const roles = (await invoke(browser, "conversation_models", { provider: "codex" }))
    .filter(({ option }) => option.selection.modelId === "desktop-fixture" && option.selectable);
  assert.equal(roles.length, 1, "The test needs the fixture Role model");
  const fixture = roles[0].option;
  const packageDocument = {
    name: title,
    members: [
      { key: "pipeline", name: title, body: { kind: "pipeline", graph: {
        name: title, nodes: [{ id: "start", kind: "typed_start", configuration: { kind: "typed_start", fields: [] } }], wires: [],
      } } },
    ],
  };
  const created = await invoke(browser, "create_workflow_package", {
    projectId, conversationId: null, package: packageDocument,
  });
  const draftId = created.id;
  packageDocument.members.push({ key: "fixture-role", name: "Desktop analyst", body: { kind: "role", template: {
    name: "Desktop analyst", role: "pipeline", model: fixture.selection, effort: null,
    instructions: "Read the supplied test inputs.", toolScope: "read_only",
    completionContract: "Return the requested result.", reviewPolicy: "independent",
  } } });
  const initialPipeline = packageDocument.members[0];
  initialPipeline.references = {};
  for (const id of ["prompt", "prompt-2"]) {
    const key = id + "-card";
    initialPipeline.body.graph.nodes.push({ id, kind: "subflow", configuration: { kind: "none" } });
    initialPipeline.references[id] = { source: "draft", key };
    packageDocument.members.push({
      key, name: id, destination: "project",
      body: { kind: "prompt", prompt: { text: "Review the candidate.", executor: "agent", inputs: {}, outputs: {}, routes: [], delegates: [] } },
      references: { role: { source: "draft", key: "fixture-role" } },
    });
  }
  const { revision: initialRevision } = await invoke(browser, "save_workflow_package", {
    projectId, draftId, expectedRevision: created.revision, package: packageDocument,
  });
  const draftScope = { projectId, draftId };
  await setWindow(browser, invoke, 1440, 900);
  await openBuilder(browser);
  await click(browser.$(`.//li[.//strong[normalize-space(.)='${title}']]//button[normalize-space(.)='Open visual draft']`));
  const editor = browser.$(builder);
  const inspector = editor.$('[aria-label="Node configuration"]');
  const promptSettings = inspector.$('[aria-label="Prompt settings"]');
  await select(inspector, "Inspect node", "prompt");
  await replaceText(browser, promptSettings.$(".//label[normalize-space(.)='Card title']/input"), "Evaluate changes");
  await replaceText(browser, promptSettings.$(".//label[normalize-space(.)='Prompt']/textarea"), "Evaluate the desktop prompt fixture.");
  await click(promptSettings.$("summary*=Inputs and outputs"));
  await click(promptSettings.$("button=Suggest with AI"));
  const suggestion = browser.$('[role="dialog"].workflow-builder-dialog');
  await click(suggestion.$('button[aria-label="Suggestion model · Choose model"]'));
  await click(browser.$(`.model-picker-menu [role="menuitemradio"][aria-label=${JSON.stringify(fixture.name)}]`));
  await click(suggestion.$("button=Request suggestions"));
  await textIsVisible(browser, suggestion.$('[aria-label="Proposed Prompt contract"]'), "review, deliver");
  await click(suggestion.$("button=Apply proposed contract"));
  await suggestion.waitForDisplayed({ timeout, reverse: true });

  await select(inspector, "Inspect node", "prompt-2");
  await replaceText(browser, promptSettings.$(".//label[normalize-space(.)='Card title']/input"), "Read summary");
  await replaceText(browser, promptSettings.$(".//label[normalize-space(.)='Prompt']/textarea"), "Read the desktop prompt fixture summary.");
  await click(promptSettings.$("summary*=Inputs and outputs"));
  await click(promptSettings.$("button=Add input"));
  await replaceText(browser, promptSettings.$(".//label[normalize-space(.)='Input 1 name']/input"), "summary");
  await browser.keys("Enter");
  await select(promptSettings, "Type for summary", "rendered_text");
  await click(promptSettings.$("summary*=Inputs and outputs"));

  const first = makeWire("start", "start", "prompt", "ready");
  const reviewed = makeWire("prompt", "review", "prompt-2", "ready");
  const data = makeWire("prompt", "summary", "prompt-2", "summary");
  await connectByKeyboard(browser, first);
  await connectByKeyboard(browser, reviewed);
  const hints = editor.$('[aria-label="Suggested data connections"]');
  await hints.waitForDisplayed({ timeout });
  assert.equal(await browser.$(edge(data)).isExisting(), false, "A suggested data wire needs confirmation");
  await click(hints.$("button*=summary"));
  for (const wire of [first, reviewed, data]) await browser.$(edge(wire)).waitForDisplayed({ timeout });
  await capture?.(browser, "visual-prompt-contract");

  await click(browser.$("button=Save draft revision"));
  let saved;
  await browser.waitUntil(async () => {
    saved = await invoke(browser, "inspect_workflow_package", draftScope);
    return saved.revision !== initialRevision;
  }, { timeout });
  const authored = saved.package.members.filter(({ body }) => body.kind === "prompt");
  assert.deepEqual(authored.map(({ name }) => name), ["Evaluate changes", "Read summary"]);
  assert.deepEqual(authored[0].body.prompt.routes, ["review", "deliver"]);
  assert.deepEqual(authored[1].body.prompt.inputs, { summary: { kind: "rendered_text" } });
  assert.deepEqual(saved.package.members[0].body.graph.wires.map(wireLabel).sort(), [first, reviewed, data].map(wireLabel).sort());
  const previousQueue = (await invoke(browser, "project_job_queue", { projectId })).jobs;

  await click(browser.$("button=Review changes and simulate"));
  const review = browser.$('[role="dialog"].workflow-builder-dialog');
  await click(review.$("summary=Prompt responses"));
  const chooseRoute = review.$('select[aria-label="Sample route for prompt::execute"]');
  async function simulateRoute(route) {
    await click(review.$("button=Simulate"));
    const trace = review.$('[aria-label="Simulation trace"]');
    await click(trace.$('[aria-label="Simulated subflows"] > summary'));
    const evaluate = trace.$(".//details[./summary[contains(normalize-space(.),'Evaluate changes')]]");
    await click(evaluate.$("./summary"));
    await textIsVisible(browser, evaluate.$('[aria-label="Forwarded outputs"]'), route + " · selected");
  }
  await chooseRoute.selectByAttribute("value", "review");
  await replaceText(browser, review.$(".//label[normalize-space(.)='summary']/input"), "Ready to review.");
  await simulateRoute("review");
  await chooseRoute.selectByAttribute("value", "deliver");
  await simulateRoute("deliver");
  assert.deepEqual((await invoke(browser, "project_job_queue", { projectId })).jobs, previousQueue);
  await capture?.(browser, "visual-prompt-simulation");
  await click(review.$('button[aria-label="Close review workflow package"]'));

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
  await select(nodeEvidence, "Inspect execution node", "prompt::execute");
  const promptRun = nodeEvidence.$('[aria-label="Prompt route and invited Roles"]');
  await textIsVisible(browser, promptRun, "Route · review");
  await textIsVisible(browser, promptRun, "The desktop fixture chose review.");
  const runs = await invoke(browser, "list_repository_deliveries", { projectId });
  const launched = runs.find(({ version }) => version.id === pipeline.id && version.revision === pipeline.revision);
  assert.ok(launched, "The accepted Prompt revision must be launched");
  const snapshot = await invoke(browser, "inspect_workflow_execution", { projectId, runId: launched.id });
  assert.equal(snapshot.launch.automatic, true);
  const outcome = snapshot.outputs.find(({ current, artifact }) => current && artifact.nodeInstanceId === "prompt::outcome"
    && artifact.port === "outcome");
  assert.equal(outcome?.artifact.value.evidence.route, "review");
  await capture?.(browser, "visual-prompt-run");

  const promptVersion = publication.members.find(({ artifact }) => artifact.name === "Evaluate changes").artifact.version;
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
  const reuseScope = { projectId, draftId: fresh.id };
  const reuseDraft = await invoke(browser, "inspect_workflow_package", reuseScope);
  const reusePipeline = reuseDraft.package.members[0];
  reusePipeline.body.graph.nodes.push({ id: "reusable-subflow", kind: "subflow", configuration: { kind: "none" } });
  reusePipeline.references = { "reusable-subflow": { source: "published", version: promptVersion } };
  const pinnedDraft = await invoke(browser, "save_workflow_package", {
    ...reuseScope, expectedRevision: reuseDraft.revision, package: reuseDraft.package,
  });
  await browser.refresh();
  await openBuilder(browser);
  await click(browser.$(`.//li[.//strong[normalize-space(.)='${reuseTitle}']]//button[normalize-space(.)='Open visual draft']`));
  const reuseInspector = reuseEditor.$('[aria-label="Node configuration"]');
  await reuseInspector.$(".//select/option[@value='reusable-subflow']").waitForExist({ timeout });
  await select(reuseInspector, "Inspect node", "reusable-subflow");
  const pinnedPrompt = reuseInspector.$('[aria-label="Prompt settings"] textarea[placeholder="Describe the work and expected result."]');
  await pinnedPrompt.waitForDisplayed({ timeout });
  assert.equal(await pinnedPrompt.getValue(), "Evaluate the desktop prompt fixture.");
  assert.equal(await pinnedPrompt.isEnabled(), false);
  await connectByKeyboard(browser, makeWire("start", "start", "reusable-subflow", "ready"));
  await click(browser.$("button=Save draft revision"));
  let reused;
  await browser.waitUntil(async () => {
    reused = await invoke(browser, "inspect_workflow_package", { projectId, draftId: fresh.id });
    return reused.revision !== pinnedDraft.revision;
  }, { timeout });
  assert.equal(reused.package.members.length, 1, "Reusing a published Prompt keeps its own immutable artifact");
  assert.deepEqual(reused.package.members[0].references["reusable-subflow"], { source: "published", version: promptVersion });
  await capture?.(browser, "visual-prompt-reused");
}
