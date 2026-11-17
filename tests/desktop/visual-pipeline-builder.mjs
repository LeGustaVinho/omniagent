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
  await browser.$(surface).scrollIntoView({ block: "end" });
  await click(browser.$(`${surface} .react-flow__controls-fitview`));
  let cards;
  try {
    await browser.waitUntil(async () => {
      cards = await browser.execute((selector, ids) => {
        const graph = document.querySelector(selector);
        return ids.map((id) => {
          const header = graph.querySelector(`.react-flow__node[data-id="${id}"] header`);
          if (!header) return { id, visible: false };
          const rect = header.getBoundingClientRect();
          const x = Math.round(rect.left + rect.width / 2);
          const y = Math.round(rect.top + rect.height / 2);
          const hit = document.elementFromPoint(x, y);
          return { id, visible: header.contains(hit), x, y, topElement: hit?.className?.toString() };
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
      await textIsVisible(browser, panel.$("h2"), "Load context");
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
        await pressEnter(browser, compactToggle);
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
  const review = browser.$('[aria-label="Review workflow package"]');
  await click(review.$("button=Review changes and simulate"));
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
  await library.$("h4=Find work").waitForDisplayed({ timeout });
  await library.$("summary=Import or export workflow packages").waitForDisplayed({ timeout });
  await click(views.$("button=Maintenance"));
  const maintenance = browser.$('[aria-label="Workflow maintenance"]');
  await maintenance.$("h3=Settings and template proposals").waitForDisplayed({ timeout });
  await maintenance.$("summary=Maintenance workflow").waitForDisplayed({ timeout });
  await click(views.$("button=Compare models"));
  const comparison = browser.$('[aria-label="Model comparisons"]');
  await comparison.$("h3=Request and context").waitForDisplayed({ timeout });
  await comparison.$("h3=Candidates").waitForDisplayed({ timeout });
  await click(views.$("button=Builder"));
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
  async function addNode(title) {
    await click(editor.$("button=Add node"));
    const search = editor.$(".//input[@aria-label='Search node catalog' or @placeholder='Search node catalog'] | .//label[normalize-space(.)='Search node catalog']/input");
    await search.waitForDisplayed({ timeout });
    await search.setValue(title);
    await click(editor.$(`button[aria-label="Add ${title}"]`));
  }
  await addNode("Load context");
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

  await addNode("Condition");
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

  await click(browser.$("button=Save package revision"));
  let saved;
  await browser.waitUntil(async () => {
    saved = await invoke(browser, "inspect_workflow_package", draftScope);
    return Boolean(saved && saved.revision !== draft.revision);
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
  await click(browser.$(`.//li[.//strong[normalize-space(.)='${title}']]//button[normalize-space(.)='Open package']`));
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

  await click(browser.$("button=Review package publication"));
  const publish = browser.$('[role="dialog"]');
  await textIsVisible(browser, publish, saved.revision);
  await click(publish.$("button=Confirm package publication"));
  const published = browser.$('[aria-label="Package publication receipt"]');
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
  return browser;
}
