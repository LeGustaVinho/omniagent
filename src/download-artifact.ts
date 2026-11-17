export function decodeArtifactBytes(base64: string) {
  return Uint8Array.from(atob(base64), (character) => character.charCodeAt(0));
}

export function downloadArtifact(name: string, content: Blob) {
  const url = URL.createObjectURL(content);
  const link = document.createElement("a");
  link.href = url;
  link.download = name;
  link.click();
  window.setTimeout(() => URL.revokeObjectURL(url), 1000);
}
