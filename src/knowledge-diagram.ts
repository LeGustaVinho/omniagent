import engineUrl from "mermaid/dist/mermaid.min.js?url&no-inline";
import rendererUrl from "@/knowledge-diagram-renderer.js?url&no-inline";

const renderRequest = "omniagent:render-mermaid";
const maxSourceLength = 50_000;
const renderTimeout = 15_000;

let frame: HTMLIFrameElement | undefined;
let ready: Promise<Window> | undefined;

function escapeAttribute(value: string) {
  return value.replaceAll("&", "&amp;").replaceAll('"', "&quot;");
}

function sandboxWindow() {
  if (frame?.isConnected && ready) return ready;
  const engine = new URL(engineUrl, window.location.href).href;
  const renderer = new URL(rendererUrl, window.location.href).href;
  const policy = [
    "default-src 'none'",
    `script-src ${engine} ${renderer}`,
    "style-src 'unsafe-inline'",
    "connect-src 'none'",
    "base-uri 'none'",
    "form-action 'none'",
  ].join("; ");

  const sandbox = document.createElement("iframe");
  sandbox.setAttribute("sandbox", "allow-scripts");
  sandbox.setAttribute("aria-hidden", "true");
  sandbox.setAttribute("referrerpolicy", "no-referrer");
  sandbox.tabIndex = -1;
  sandbox.title = "Mermaid renderer";
  // Allow the engine to measure SVG text without exposing its transient DOM to the user.
  sandbox.style.cssText = "position:fixed;top:0;left:0;width:1024px;height:768px;visibility:hidden;pointer-events:none;border:0";
  sandbox.srcdoc = `<!doctype html><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="${escapeAttribute(policy)}"><script src="${escapeAttribute(engine)}" defer></script><script src="${escapeAttribute(renderer)}" defer></script>`;
  ready = new Promise<Window>((resolve, reject) => {
    sandbox.addEventListener("load", () => {
      if (sandbox.contentWindow) resolve(sandbox.contentWindow);
      else reject(new Error("The Mermaid renderer could not start."));
    }, { once: true });
  });
  frame = sandbox;
  document.body.append(sandbox);
  return ready;
}

export async function renderNoteDiagram(source: string, signal: AbortSignal): Promise<string> {
  if (source.length > maxSourceLength) throw new Error("The Mermaid diagram exceeds the source size limit.");
  signal.throwIfAborted();

  return new Promise((resolve, reject) => {
    const { port1, port2 } = new MessageChannel();
    let settled = false;
    const cleanup = () => {
      settled = true;
      window.clearTimeout(timer);
      signal.removeEventListener("abort", abort);
      port1.close();
    };
    const fail = (error: unknown) => {
      cleanup();
      reject(error);
    };
    const abort = () => fail(signal.reason);
    const timer = window.setTimeout(() => fail(new Error("The Mermaid renderer did not respond.")), renderTimeout);
    signal.addEventListener("abort", abort, { once: true });
    port1.onmessage = ({ data }) => {
      if (typeof data?.svg === "string" && /^<svg\b/.test(data.svg)) {
        try {
          const image = "data:image/svg+xml," + encodeURIComponent(data.svg);
          cleanup();
          resolve(image);
        } catch (error) {
          fail(error);
        }
      } else {
        fail(new Error("The Mermaid diagram could not be rendered."));
      }
    };
    void sandboxWindow().then((target) => {
      // The sandbox has an opaque origin. Only the transferred port carries a reply.
      if (settled) return;
      target.postMessage({ type: renderRequest, source }, "*", [port2]);
    }).catch(fail);
  });
}
