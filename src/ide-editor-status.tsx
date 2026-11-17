import { ContextActions } from "@/components/ui/context-actions";
import type { EditorSettings } from "@/ide-code-editor";
import { lineEndingLabels, savedEncoding, sourceEncodingLabels, type FileFormat, type SourceEncoding, type SourceFormatChange } from "@/source-file-format";

const indentationSizes = [2, 4, 8];
const encodings = Object.keys(sourceEncodingLabels) as SourceEncoding[];
const selectableLineEndings = ["lf", "crlf", "cr"] as const;

export function EditorStatus({ path, saved, settings, format, copy, onChangeFormat, canConvert = saved }: {
  path: string;
  saved: boolean;
  settings?: EditorSettings;
  format?: FileFormat;
  copy: (text: string, status: string) => void;
  onChangeFormat?: (change: SourceFormatChange) => void;
  canConvert?: boolean;
}) {
  const encodingId = savedEncoding(format);
  const encoding = sourceEncodingLabels[encodingId];
  const lineEndings = format && lineEndingLabels[format.lineEndings];
  const language = settings?.languages.find(({ id }) => settings.language === id)?.label ?? settings?.language;
  return <div className="ide-status-details">
    <span>{saved ? "Saved" : "Unsaved changes"}</span>
    {settings && <>
      <ContextActions label="editor language" groups={[[
        { label: "Detect from filename", disabled: settings.language === settings.autoLanguage, run: () => settings.setLanguage(settings.autoLanguage) },
      ], settings.languages.map(({ id, label }) => ({ label, disabled: settings.language === id, run: () => settings.setLanguage(id) }))]}>
        <span className="ide-status-setting" tabIndex={0} aria-label={`Editor language ${language}`}>{language}</span>
      </ContextActions>
      <ContextActions label="editor indentation" groups={[
        [{ label: "Detect indentation from content", run: settings.detectIndentation }],
        indentationSizes.map((size) => ({ label: `Indent using ${size} spaces`, disabled: settings.insertSpaces && settings.tabSize === size,
          run: () => settings.setIndentation(size, true) })),
        indentationSizes.map((size) => ({ label: `Indent using tabs (${size} columns)`, disabled: !settings.insertSpaces && settings.tabSize === size,
          run: () => settings.setIndentation(size, false) })),
      ]}>
        <span className="ide-status-setting" tabIndex={0}>{settings.insertSpaces ? `Spaces: ${settings.tabSize}` : `Tab size: ${settings.tabSize}`}</span>
      </ContextActions>
    </>}
    <ContextActions label="saved file format" groups={[onChangeFormat && format ? [
      { label: "Change encoding", disabled: !canConvert, items: encodings.map((value) => ({
        label: sourceEncodingLabels[value], disabled: value === encodingId, run: () => onChangeFormat({ encoding: value }),
      })) },
      { label: "Convert line endings", disabled: !canConvert || format.lineEndings === "none", items: selectableLineEndings.map((value) => ({
        label: lineEndingLabels[value], disabled: value === format.lineEndings, run: () => onChangeFormat({ lineEndings: value }),
      })) },
    ] : [], [
      { label: "Copy encoding", run: () => copy(encoding, "Copied encoding") },
      ...(lineEndings ? [{ label: "Copy line ending format", run: () => copy(lineEndings, "Copied line ending format") }] : []),
      { label: "Copy saved file format", run: () => copy([path, encoding, lineEndings].filter(Boolean).join(" · "), "Copied saved file format") },
    ]]}>
      <span className="ide-status-setting" tabIndex={0} aria-label={`Saved file format ${encoding}${lineEndings ? `, ${lineEndings}` : ""}`}>
        {encoding}{lineEndings && ` · ${lineEndings}`}
      </span>
    </ContextActions>
  </div>;
}
