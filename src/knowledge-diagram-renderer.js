// This classic script runs only in the opaque iframe. It receives no application state.
(() => {
  const request = "omniagent:render-mermaid";
  const maxSourceLength = 50_000;
  const engine = globalThis.mermaid;
  let sequence = 0;
  if (window === window.parent || !engine) return;

  engine.initialize({
    startOnLoad: false,
    securityLevel: "strict",
    htmlLabels: false,
    suppressErrorRendering: true,
    theme: "neutral",
    maxTextSize: maxSourceLength,
  });

  function cleanSource(source) {
    const cleaned = source.replaceAll(/\r\n?/g, "\n").trimStart()
      .replace(/^---[^\S\n]*\n[\s\S]*?\n[^\S\n]*---[^\S\n]*(?:\n|$)/, "")
      .replace(/%%\{[\s\S]*?\}%%/g, "")
      .trimStart();
    if (cleaned.includes("%%{") || /^---(?:\s|$)/.test(cleaned)) {
      throw new Error("Invalid Mermaid configuration directive.");
    }
    return cleaned;
  }

  function inertSvg(markup) {
    // Mermaid serializes HTML's DOM. A detached template parses that fragment without
    // installing it or activating any of its resource references.
    const template = document.createElement("template");
    template.innerHTML = markup;
    const svg = template.content.firstElementChild;
    if (svg?.localName !== "svg" || svg.namespaceURI !== "http://www.w3.org/2000/svg") {
      throw new Error("Invalid Mermaid image.");
    }
    for (const element of svg.querySelectorAll("script, foreignObject, image, iframe, object, embed, animate, set, animateMotion, animateTransform")) {
      element.remove();
    }
    for (const element of [svg, ...svg.querySelectorAll("*")]) {
      for (const attribute of [...element.attributes]) {
        const name = attribute.localName.toLowerCase();
        if (name.startsWith("on") || name === "src" || (name === "href" && !attribute.value.startsWith("#"))) {
          element.removeAttributeNode(attribute);
        }
      }
    }
    return new XMLSerializer().serializeToString(svg);
  }

  window.addEventListener("message", async (event) => {
    const { type, source } = event.data ?? {};
    const [reply] = event.ports;
    if (event.source !== window.parent || type !== request || !reply) return;
    try {
      if (typeof source !== "string" || source.length > maxSourceLength) throw new Error("Invalid Mermaid source.");
      const result = await engine.render(`note-diagram-${++sequence}`, cleanSource(source));
      reply.postMessage({ svg: inertSvg(result.svg) });
    } catch {
      reply.postMessage({ error: true });
    } finally {
      reply.close();
    }
  });
})();
