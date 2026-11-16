// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import rendererScript from "@/knowledge-diagram-renderer.js?raw";

const request = "omniagent:render-mermaid";

function renderer(svg: string) {
  const parent = {};
  const engine = {
    initialize: vi.fn(),
    render: vi.fn(async (_id: string, _source: string) => ({ svg })),
  };
  type Incoming = {
    source: object;
    data: { type: string; source: string };
    ports: { postMessage: (data: unknown) => void; close: () => void }[];
  };
  let respond: ((message: Incoming) => Promise<void>) | undefined;
  const sandbox = {
    parent,
    addEventListener: (name: string, handler: typeof respond) => {
      if (name === "message") respond = handler;
    },
  };
  new Function("window", "globalThis", "document", "XMLSerializer", rendererScript)(
    sandbox, { mermaid: engine }, document, XMLSerializer,
  );
  async function send(source: string, sender = parent) {
    const reply = { postMessage: vi.fn(), close: vi.fn() };
    await respond?.({ source: sender, data: { type: request, source }, ports: [reply] });
    return reply;
  }
  return { engine, send };
}

describe("Mermaid sandbox renderer", () => {
  it("keeps configuration locked and strips front matter and deprecated directives", async () => {
    const sandbox = renderer('<svg xmlns="http://www.w3.org/2000/svg"><text>Fixture</text></svg>');
    const diagram = "flowchart TD\n  A[Fixture] --> B[Finish]";
    const source = "\uFEFF  ---\r\nconfig:\r\n  securityLevel: loose\r\n  htmlLabels: true\r\n---\r\n"
      + "%%{init: {'securityLevel': 'loose', 'htmlLabels': true}}%%\r\n"
      + diagram.replaceAll("\n", "\r\n");
    const reply = await sandbox.send(source);
    expect(sandbox.engine.initialize).toHaveBeenCalledWith(expect.objectContaining({
      securityLevel: "strict", htmlLabels: false, startOnLoad: false,
    }));
    expect(sandbox.engine.render).toHaveBeenCalledWith("note-diagram-1", diagram);
    expect(reply.postMessage).toHaveBeenCalledWith({ svg: expect.stringContaining("<text>Fixture</text>") });
    expect(reply.close).toHaveBeenCalledOnce();
  });

  it("rejects unclosed directives, parse failures, and messages from other windows", async () => {
    const sandbox = renderer('<svg xmlns="http://www.w3.org/2000/svg"/>');
    const foreign = await sandbox.send("flowchart TD\n A --> B", {});
    expect(foreign.postMessage).not.toHaveBeenCalled();
    const directive = await sandbox.send("%%{init: {'securityLevel': 'loose'}\nflowchart TD\n A --> B");
    expect(directive.postMessage).toHaveBeenCalledWith({ error: true });
    expect(sandbox.engine.render).not.toHaveBeenCalled();
    sandbox.engine.render.mockRejectedValueOnce(new Error("private renderer detail"));
    const invalid = await sandbox.send("bad diagram source");
    expect(invalid.postMessage).toHaveBeenCalledWith({ error: true });
  });

  it("removes active markup and external references from the SVG payload", async () => {
    const sandbox = renderer(`<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" onload="invoke()">
      <script>invoke()</script>
      <foreignObject><img xmlns="http://www.w3.org/1999/xhtml" onerror="invoke()" src="https://example.invalid/fixture" /></foreignObject>
      <a xlink:href="javascript:invoke()"><text>Hello</text></a>
      <image href="https://example.invalid/diagram.png"/><use href="#arrow"/><path id="arrow"/>
      <animate attributeName="href" to="javascript:invoke()"/>
    </svg>`);
    const reply = await sandbox.send("flowchart TD\n A --> B");
    const svg = reply.postMessage.mock.calls[0]?.[0]?.svg as string;
    expect(svg).toContain("<text>Hello</text>");
    expect(svg).toContain('href="#arrow"');
    expect(svg).not.toMatch(/<script|<foreignObject|<image|<animate|onload|onerror|javascript:|example\.invalid/);
  });
});

describe("Mermaid host boundary", () => {
  afterEach(() => {
    document.querySelector("iframe[title='Mermaid renderer']")?.remove();
    vi.unstubAllGlobals();
  });

  it("passes source over a transferred port and receives only an image URL", async () => {
    const port1 = { onmessage: (_message: { data: unknown }) => {}, close: vi.fn() };
    const port2 = { close: vi.fn() };
    vi.stubGlobal("MessageChannel", class { port1 = port1; port2 = port2; });
    const { renderNoteDiagram } = await import("@/knowledge-diagram");
    const source = 'flowchart TD\n A["<script>unsafe()</script>"] --> B[Done]';
    const result = renderNoteDiagram(source, new AbortController().signal);
    const frame = document.querySelector<HTMLIFrameElement>("iframe[title='Mermaid renderer']")!;
    const send = vi.spyOn(frame.contentWindow!, "postMessage");
    expect(frame.getAttribute("sandbox")).toBe("allow-scripts");
    expect(frame.srcdoc).toContain("connect-src 'none'");
    expect(frame.srcdoc).not.toContain("unsafe()");
    expect(frame.srcdoc).not.toMatch(/allow-same-origin|tauri_internals|ipc\.localhost/i);
    frame.dispatchEvent(new Event("load"));
    await Promise.resolve();
    expect(send).toHaveBeenCalledWith({ type: request, source }, "*", [port2]);
    port1.onmessage({ data: { svg: '<svg xmlns="http://www.w3.org/2000/svg"/>' } });
    await expect(result).resolves.toMatch(/^data:image\/svg\+xml,/);
    expect(document.querySelector("svg")).toBeNull();
  });
});
