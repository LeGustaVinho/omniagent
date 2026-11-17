import { useState } from "react";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { Provider } from "@/bindings/Provider";
import { ModelPicker } from "@/components/model-picker";
import { Button } from "@/components/ui/button";
import { ProviderSettings } from "@/provider-settings";
import { QualificationMatrix } from "@/qualification-matrix";
import { IntegrationNotice, SettingRow } from "@/settings-controls";
import { exampleSettingsModels } from "@/settings-models";
import type { WorkspaceProps } from "@/workbench-contract";

const providers: { id: Provider; label: string; command: string }[] = [
  { id: "codex", label: "Codex", command: "codex.exe" },
  { id: "claude", label: "Claude Code", command: "claude.exe" },
  { id: "antigravity", label: "Antigravity", command: "agy.exe" },
];

type ExampleProvider = {
  path: string;
  model: ModelSelection | null;
  runner: "direct" | "guest";
  checked: boolean;
  verified: boolean;
  inventoryVisible: boolean;
};

const defaultProfile: ExampleProvider = {
  path: "",
  model: null,
  runner: "direct",
  checked: false,
  verified: false,
  inventoryVisible: false,
};

export function useProviderExamples() {
  const [profiles, setProfiles] = useState<Partial<Record<Provider, ExampleProvider>>>({});
  return {
    profiles,
    update: (provider: Provider, options: Partial<ExampleProvider>) => setProfiles((current) => ({
      ...current,
      [provider]: { ...defaultProfile, ...current[provider], ...options },
    })),
  };
}

type Examples = ReturnType<typeof useProviderExamples>;

export function ProviderProfiles({
  examples,
  start,
  checkoutId,
}: {
  examples?: Examples;
  start?: WorkspaceProps["startSimulation"];
  checkoutId?: string;
}) {
  if (examples) return <ProviderExamples {...examples} />;
  return (
    <>
      <ProviderSettings checkoutId={checkoutId} />
      <section className="settings-system-group" data-setting="runner">
        <h3>Runners and capabilities</h3>
        <SettingRow id="linked-runner" label="Execution runner" description="Runner qualification includes the host build, checkout scope and effective filesystem permissions.">
          <select id="linked-runner" disabled defaultValue="direct">
            <option value="direct">Direct Windows host</option>
            <option value="guest">Hyper-V guest</option>
          </select>
        </SettingRow>
        <QualificationMatrix validationCheckoutId={checkoutId} />
        <p>Validation is available on eligible conversation attempts in the active checkout.</p>
        <IntegrationNotice start={start}>
          Linked Hyper-V guests will appear here when available.
        </IntegrationNotice>
      </section>
    </>
  );
}

function ProviderExamples({ profiles, update }: Examples) {
  const [selected, setSelected] = useState<Provider>("codex");
  const [confirmCheck, setConfirmCheck] = useState(false);
  const provider = providers.find(({ id }) => id === selected)!;
  const profile = profiles[selected] ?? defaultProfile;

  return (
    <div className="settings-system">
      <SettingRow id="provider-paths" label="Provider">
        <select id="provider-paths" value={selected} onChange={(event) => {
          setSelected(event.target.value as Provider);
          setConfirmCheck(false);
        }}>
          {providers.map(({ id, label }) => <option key={id} value={id}>{label}</option>)}
        </select>
      </SettingRow>
      <ExecutableExample key={selected} profile={profile} command={provider.command}
        save={(path) => {
          update(selected, { path, checked: false, verified: false });
          setConfirmCheck(false);
        }} />
      <SettingRow id="runner" label="Execution runner" description="Choose the example Windows environment for CLI checks.">
        <select id="runner" value={profile.runner} onChange={(event) => {
          update(selected, { runner: event.target.value as ExampleProvider["runner"], checked: false, verified: false });
          setConfirmCheck(false);
        }}>
          <option value="direct">Direct Windows host</option>
          <option value="guest">Hyper-V guest</option>
        </select>
      </SettingRow>
      <div className="settings-actions">
        <Button type="button" variant="outline" onClick={() => update(selected, { checked: true })}>Check CLI</Button>
        <Button type="button" variant="outline" onClick={() => update(selected, { inventoryVisible: true })}>Check plugins</Button>
      </div>
      <dl className="settings-facts">
        <dt>Installation</dt><dd>{profile.checked ? "Executable detected" : "Unchecked"}</dd>
        <dt>Protocol</dt><dd>{profile.checked ? "Negotiated" : "Unchecked"}</dd>
        <dt>Authentication</dt><dd>{profile.checked ? "Signed in for this example" : "Unverified"}</dd>
        <dt>Filesystem scope</dt><dd>Selected checkout · read-only</dd>
        <dt>Windows runner</dt><dd>{profile.runner === "direct" ? "Direct · Windows 11 x64" : "Hyper-V guest · Windows 10 x64"}</dd>
        <dt>Capability verification</dt><dd>{profile.verified ? "Verified in disposable fixture" : "Pending"}</dd>
      </dl>
      {profile.inventoryVisible && (
        <details className="settings-matrix" open>
          <summary>Installed plugins (1)</summary>
          <p><strong>Review toolkit</strong> · 1.1.0 · Enabled</p>
          <p>Skills · Review changes, summarize logs</p>
          <p>MCP servers · Repository inspector</p>
        </details>
      )}
      <div data-setting="capabilities">
        <ModelPicker label="Qualification model"
          options={exampleSettingsModels.filter(({ selection }) => selection.provider === selected)}
          selection={{ mode: "single", selected: profile.model, onChange: (model) => {
            update(selected, { model, verified: false });
            setConfirmCheck(false);
          } }} />
        <QualificationMatrix examples />
        <div className="settings-actions">
          {!confirmCheck && <Button type="button" variant="outline" disabled={!profile.checked || !profile.model || profile.verified}
            onClick={() => setConfirmCheck(true)}>Validate capability</Button>}
        </div>
        {confirmCheck && (
          <section className="settings-matrix" aria-label="Qualification fixture">
            <h3>Qualification fixture</h3>
            <p>Check {exampleSettingsModels.find(({ selection }) => selection.modelId === profile.model?.modelId)?.name} using a disposable worktree and an approval prompt.</p>
            <div className="settings-actions">
              <Button type="button" onClick={() => {
                update(selected, { verified: true });
                setConfirmCheck(false);
              }}>Approve fixture check</Button>
              <Button type="button" variant="outline" onClick={() => setConfirmCheck(false)}>Cancel</Button>
            </div>
          </section>
        )}
      </div>
    </div>
  );
}

function ExecutableExample({
  profile,
  command,
  save,
}: {
  profile: ExampleProvider;
  command: string;
  save: (path: string) => void;
}) {
  const [path, setPath] = useState(profile.path);
  return (
    <form className="settings-action-form" onSubmit={(event) => {
      event.preventDefault();
      save(path.trim());
      setPath(path.trim());
    }}>
      <SettingRow id="example-executable" label="Executable path" description={`PATH lookup uses ${command}.`}>
        <input id="example-executable" type="text" value={path} placeholder="Use PATH"
          onChange={(event) => setPath(event.target.value)} />
      </SettingRow>
      <div className="settings-save">
        <Button type="submit" disabled={path.trim() === profile.path}>Save</Button>
        <Button type="button" variant="outline" disabled={path === "" && profile.path === ""} onClick={() => {
          save("");
          setPath("");
        }}>Use PATH</Button>
      </div>
    </form>
  );
}
