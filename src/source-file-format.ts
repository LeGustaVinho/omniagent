export const sourceEncodingLabels = {
  utf8: "UTF-8",
  utf8_bom: "UTF-8 with BOM",
  utf16_le: "UTF-16 LE with BOM",
  utf16_be: "UTF-16 BE with BOM",
};
export const lineEndingLabels = { none: "No line breaks", lf: "LF", crlf: "CRLF", cr: "CR", mixed: "Mixed line endings" };

export type SourceEncoding = keyof typeof sourceEncodingLabels;
export type LineEndings = keyof typeof lineEndingLabels;
export type FileFormat = { utf8Bom: boolean; encoding?: SourceEncoding; lineEndings: LineEndings };
export type SourceFormatChange = { encoding?: SourceEncoding; lineEndings?: "lf" | "crlf" | "cr" };
export type SourceFormatPreview = {
  originalEncoding: SourceEncoding;
  originalLineEndings: LineEndings;
  encoding: SourceEncoding;
  lineEndings: LineEndings;
  originalBytes: number;
  convertedBytes: number;
  originalPrefix: string;
  convertedPrefix: string;
  sha256: string;
};
export type ReviewedSourceFormat = {
  path: string;
  expectedRevision: string;
  change: SourceFormatChange;
  preview: SourceFormatPreview;
};

export function savedEncoding(format?: Pick<FileFormat, "encoding" | "utf8Bom">): SourceEncoding {
  return format?.encoding ?? (format?.utf8Bom ? "utf8_bom" : "utf8");
}
