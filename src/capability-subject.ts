import type { Capability } from "@/bindings/Capability";

export type CapabilitySubject =
  | { kind: "protocol"; capability: Capability }
  | { kind: "tool"; id: string; version: string; operation: string }
  | { kind: "native_attachment"; format: string; encoding: string }
  | { kind: "attachment_limits" };

export function capabilitySubjectLabel(subject: CapabilitySubject) {
  switch (subject.kind) {
    case "protocol": return subject.capability.replaceAll("_", " ");
    case "tool": return `${subject.id} ${subject.version} · ${subject.operation}`;
    case "native_attachment": return `${subject.format.replaceAll("_", " ")} · ${subject.encoding.replaceAll("_", " ")}`;
    case "attachment_limits": return "Attachment limits";
  }
}
