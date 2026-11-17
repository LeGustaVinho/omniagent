import { ArrowLeft, Bot, Braces, ChevronRight, Cpu, GitPullRequest, HardDrive, Monitor, Moon, Paperclip, Search, Settings2, Shield, SlidersHorizontal, Sun, Workflow, X, type LucideIcon } from "lucide-react";
import { Dialog } from "radix-ui";
import { useEffect, useRef, useState } from "react";
import type { AppearanceSettings } from "@/bindings/AppearanceSettings";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";
import { CandidateGateSettings } from "@/candidate-gate-settings";
import { deliveryGitSetting } from "@/delivery-git-policy";
import { DeliveryGitSettings, SimulatedDeliveryGitSettings } from "@/delivery-git-settings";
import { Button } from "@/components/ui/button";
import { ProjectQueueHistoryPolicy } from "@/conversation-history-controls";
import { CustomizationsSettings } from "@/customizations-panel";
import { ProjectModelSettings, projectModelSettings } from "@/model-default-settings";
import { ProjectAttachmentSettings } from "@/provider-settings";
import { defaultAppearance, useAppearancePresentation, useEditableAppearance } from "@/settings-appearance";
import { SettingRow } from "@/settings-controls";
import { PolicyForm, policyFields } from "@/settings-policies";
import { ProviderProfiles, useProviderExamples } from "@/settings-provider-profiles";
import { ComposerSuggestionSettings } from "@/conversation-side-requests";
import { SystemSettings, useMaintenanceSimulation } from "@/settings-system";
import { ProjectSubagentControls } from "@/subagent-quota";
import { ProjectSubagentAuditControl } from "@/subagent-audit-controls";
import type { WorkspaceProps } from "@/workbench-contract";
import { WorkflowSettingsProposals } from "@/workflow-settings-proposals";
import { WorkflowMaintenanceSettings } from "@/workflow-maintenance";
import "./settings-dialog.css";

type SettingsDialogProps = Omit<WorkspaceProps, "checkout"> & {
  checkout: ProjectCheckout | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  initialPage?: string;
};

type Scope = "device" | "project" | "checkout";
type PageId = "appearance" | "providers" | "capacity" | "system" | "attachments" | "execution" | "conversation" | "git" | "automation" | "customizations";
type SearchEntry = { id: string; label: string; description?: string };
type Page = { id: PageId; scope: Scope; title: string; icon: LucideIcon; entries: readonly SearchEntry[] };

const appearanceEntries: Record<keyof AppearanceSettings, SearchEntry> = {
  theme: { id: "theme", label: "Theme", description: "Dark, light or the Windows color scheme." },
  interfaceFont: { id: "interface-font", label: "Interface font", description: "Geist or Segoe UI for menus and controls." },
  responseFont: { id: "response-font", label: "Response font", description: "Match the interface or use Spectral." },
  conversationTextSize: { id: "conversation-text-size", label: "Conversation text size", description: "Text in the transcript and composer." },
  transcriptWidth: { id: "transcript-width", label: "Transcript width", description: "Maximum width of a conversation’s reading column." },
  motion: { id: "motion", label: "Motion", description: "Follow Windows or reduce animation." },
};

const pages: readonly Page[] = [
  { id: "appearance", scope: "device", title: "Appearance", icon: Monitor, entries: Object.values(appearanceEntries) },
  { id: "providers", scope: "device", title: "Providers", icon: Bot, entries: [
    { id: "provider-paths", label: "Executable paths", description: "Codex, Claude Code, Gemini, Antigravity, PATH, authentication, CLI protocol and plugin inventory" },
    { id: "runner", label: "Runners and capabilities", description: "Direct Windows host, Hyper-V guest, sandbox, verified capability profile" },
  ] },
  { id: "capacity", scope: "device", title: "Execution capacity", icon: Cpu, entries: policyFields.capacity },
  { id: "system", scope: "device", title: "System", icon: HardDrive, entries: [
    { id: "environment", label: "Current installation", description: "Build, local data directory, runtime" },
    { id: "qualification", label: "Prerequisites and qualification", description: "Windows, WebView2, Git for Windows, ripgrep, CLI, model, runner" },
    { id: "backup", label: "Backup and restore", description: "SQLite, protected history, manifest, drafts, snapshot and recovery" },
    { id: "updates", label: "Updates and recovery", description: "Signed package, installer, update, restart and rollback" },
  ] },
  { id: "attachments", scope: "project", title: "Attachments", icon: Paperclip, entries: [
    { id: "attachment-limits", label: "Attachment limits", description: "Files per turn, native and staged bytes per file and turn, UTF-8 excerpt size" },
  ] },
  { id: "execution", scope: "project", title: "Execution policies", icon: Shield, entries: policyFields.execution },
  { id: "conversation", scope: "project", title: "Conversation defaults", icon: SlidersHorizontal, entries: [projectModelSettings.conversation, ...policyFields.conversation] },
  { id: "git", scope: "project", title: "Git and delivery", icon: GitPullRequest, entries: [
    deliveryGitSetting,
    ...policyFields.git,
    { id: "candidate-gates", label: "Candidate verification checks", description: "Build commands, test commands and required checks" },
  ] },
  { id: "automation", scope: "project", title: "Workflow automation", icon: Workflow, entries: policyFields.automation },
  { id: "customizations", scope: "checkout", title: "Customizations", icon: Braces, entries: [
    { id: "customization-inventory", label: "Repository inventory", description: "Hooks, MCP servers, plugin declarations, skills, Markdown instructions and approved revisions" },
  ] },
];

function ProjectQuestionTimeout({ projectId }: { projectId: string }) {
  const client = useQueryClient();
  const key = ["project-question-timeout", projectId];
  const timeout = useQuery({
    queryKey: key,
    queryFn: () => invoke<number>("project_question_timeout", { projectId }),
  });
  const [draft, setDraft] = useState<string>();
  const save = useMutation({
    mutationFn: (minutes: number) => invoke<void>("set_project_question_timeout", { projectId, minutes }),
    onSuccess: (_, minutes) => {
      client.setQueryData(key, minutes);
      setDraft(undefined);
    },
  });
  return <SettingRow id="live-wait" label="Live question timeout (minutes)"
    description="After this time, OmniAgent saves the question and stops the live turn.">
    <form className="flex flex-wrap items-center gap-2" onSubmit={(event) => {
      event.preventDefault();
      if (draft !== undefined) save.mutate(Number(draft));
    }}>
      <input id="live-wait" className="w-20 rounded-md border border-input bg-background px-2 py-1.5 text-sm"
        aria-describedby="live-wait-description" type="number" min={1} max={120} required
        disabled={timeout.data === undefined || save.isPending}
        value={draft ?? timeout.data ?? ""} onChange={(event) => setDraft(event.target.value)} />
      <Button type="submit" variant="outline" disabled={draft === undefined || save.isPending}>Save</Button>
      {(timeout.error || save.error) && <p role="alert" className="w-full text-sm text-red-300">{String(save.error || timeout.error)}</p>}
    </form>
  </SettingRow>;
}

const groups: { scope: Scope; label: string }[] = [
  { scope: "device", label: "This device" },
  { scope: "project", label: "Project" },
  { scope: "checkout", label: "Checkout" },
];

function pageFromLink(initialPage?: string) {
  return initialPage !== undefined && initialPage !== "appearance";
}

function initialSelection(initialPage?: string): PageId {
  if (initialPage === "provider-settings") return "providers";
  if (initialPage === "updates" || initialPage === "backup") return "system";
  return pages.find(({ id }) => id === initialPage)?.id ?? "appearance";
}

function initialAnchor(initialPage?: string) {
  return initialPage === "backup" || initialPage === "updates" ? initialPage : undefined;
}

type SearchResult = {
  page: Page;
  entry?: SearchEntry;
};

function searchSettings(available: readonly Page[], query: string): SearchResult[] {
  const terms = query.trim().toLowerCase().split(/\s+/);
  function matches(haystack: string) {
    const text = haystack.toLowerCase();
    return terms.every((term) => text.includes(term));
  }

  return available.flatMap((page) => {
    const pageResult: SearchResult[] = matches(page.title) ? [{ page }] : [];
    const controlResults = page.entries.filter(({ label, description }) =>
      matches(`${page.title} ${label} ${description ?? ""}`)).map((entry) => ({ page, entry }));
    return [...pageResult, ...controlResults];
  });
}

function basename(path: string) {
  return path.replace(/[\\/]+$/, "").split(/[\\/]/).at(-1) || path;
}

function scopeTarget(scope: Scope, checkout: ProjectCheckout | null) {
  if (!checkout || scope === "device") return { label: "This device", path: undefined };
  const path = scope === "project" ? checkout.repositoryRoot : checkout.checkoutRoot;
  return { label: `${scope === "project" ? "Project" : "Checkout"} · ${basename(path)}`, path };
}

export default function SettingsDialog(props: SettingsDialogProps) {
  return <SettingsSession key={props.simulation ? `simulation-${props.simulation.epoch}` : "live"} {...props} />;
}

function SettingsSession({
  checkout,
  open,
  onOpenChange,
  openActivity,
  initialPage,
  simulation,
  startSimulation,
}: SettingsDialogProps) {
  const [selection, setSelection] = useState<{ page: PageId; anchor?: string }>(() => ({
    page: initialSelection(initialPage), anchor: initialAnchor(initialPage),
  }));
  const [mobilePage, setMobilePage] = useState(() => pageFromLink(initialPage));
  const [query, setQuery] = useState("");
  const [exampleAppearance, setExampleAppearance] = useState(defaultAppearance);
  const [disposablePolicies, setDisposablePolicies] = useState<Partial<Record<PageId, Record<string, string>>>>({});
  const [approvedExample, setApprovedExample] = useState(false);
  const providerExamples = useProviderExamples();
  const maintenance = useMaintenanceSimulation();
  const { preferences, save } = useEditableAppearance(open && !simulation);
  const appearance = (simulation ? exampleAppearance : preferences.data) ?? defaultAppearance;
  const presentation = useAppearancePresentation(appearance);
  const searchInput = useRef<HTMLInputElement>(null);
  const pageTitle = useRef<HTMLHeadingElement>(null);
  const content = useRef<HTMLDivElement>(null);
  const previouslyFocused = useRef<HTMLElement | null>(null);
  const showResults = query.trim().length > 0;
  const available = checkout ? pages : pages.filter(({ scope }) => scope === "device");
  const currentPage = (available.find(({ id }) => id === selection.page) ?? pages[0])!;
  const target = scopeTarget(currentPage.scope, checkout);
  const results = showResults ? searchSettings(available, query) : [];
  const simulate = checkout ? startSimulation : undefined;

  useEffect(() => {
    if (!open) return;
    setSelection({ page: initialSelection(initialPage), anchor: initialAnchor(initialPage) });
    setQuery("");
    setMobilePage(pageFromLink(initialPage));
  }, [initialPage, open]);

  useEffect(() => {
    const panel = content.current;
    if (!open || !panel) return;
    if (!selection.anchor) {
      panel.scrollTop = 0;
      return;
    }
    const setting = panel.querySelector<HTMLElement>(`[data-setting="${selection.anchor}"]`);
    setting?.scrollIntoView?.({ block: "start" });
    const focus = setting?.querySelector<HTMLElement>("button:not([disabled]), input:not([disabled]), select:not([disabled]), summary")
      ?? (setting?.hasAttribute("tabindex") ? setting : pageTitle.current);
    focus?.focus({ preventScroll: true });
  }, [open, selection]);

  function navigate(page: PageId, anchor?: string) {
    setSelection({ page, anchor });
    setMobilePage(true);
    if (!anchor && window.matchMedia?.("(max-width: 759px)").matches) {
      requestAnimationFrame(() => pageTitle.current?.focus());
    }
  }

  function changeAppearance<Key extends keyof AppearanceSettings>(key: Key, value: AppearanceSettings[Key]) {
    if (!simulation && save.isPending) return;
    const updated = { ...appearance, [key]: value };
    if (simulation) setExampleAppearance(updated);
    else save.mutate(updated);
  }

  function policy(id: keyof typeof policyFields, fieldPage: PageId = id) {
    return <PolicyForm key={fieldPage}
      fields={policyFields[id]} stored={disposablePolicies[fieldPage]} simulate={simulate}
      save={simulation ? (value) => setDisposablePolicies((current) => ({ ...current, [fieldPage]: value })) : undefined} />;
  }

  function pageContent() {
    switch (currentPage.id) {
      case "appearance":
        return <>
          {!simulation && preferences.isError && <p className="settings-result settings-result-error" role="alert">
            Appearance preferences could not be loaded. <button type="button" onClick={() => void preferences.refetch()}>Retry</button>
          </p>}
          {!simulation && save.isError && <p className="settings-result settings-result-error" role="alert">
            Appearance preference could not be saved. The last saved value was restored.
          </p>}
          {!simulation && save.isSuccess && <span className="sr-only" role="status">Appearance saved</span>}
          <fieldset disabled={!simulation && (preferences.isPending || preferences.isError)}>
            <AppearanceControls settings={appearance} update={changeAppearance} />
          </fieldset>
        </>;
      case "providers":
        return <ProviderProfiles examples={simulation ? providerExamples : undefined} start={simulate} />;
      case "attachments":
        return simulation ? policy("attachments") : checkout && <ProjectAttachmentSettings key={checkout.projectId} projectId={checkout.projectId} />;
      case "customizations":
        return simulation
          ? <CustomizationExample approved={approvedExample} approve={() => setApprovedExample(true)} />
          : <CustomizationsSettings key={checkout?.checkoutId} checkoutId={checkout?.checkoutId} embedded />;
      case "system":
        return <>
          <SystemSettings simulated={simulation ? maintenance : undefined} startSimulation={simulate} />
          {!simulation && checkout && <WorkflowMaintenanceSettings key={checkout.projectId} checkout={checkout} />}
        </>;
      case "conversation":
        if (simulation || !checkout) return policy("conversation");
        return <>
          <ProjectModelSettings key={`conversation-model-${checkout.projectId}`} projectId={checkout.projectId} purposes={["conversation"]} />
          <ProjectQueueHistoryPolicy key={checkout.projectId} projectId={checkout.projectId} />
          <ComposerSuggestionSettings />
          <PolicyForm fields={policyFields.conversation.filter(({ id }) => id !== "queued-context" && id !== "suggestions")} stored={undefined} simulate={simulate} />
        </>;
      case "execution":
        if (simulation || !checkout) return policy("execution");
        return <>
          <ProjectModelSettings key={`work-models-${checkout.projectId}`} projectId={checkout.projectId} purposes={["planning", "implementation"]} />
          <ProjectSubagentControls key={checkout.projectId} projectId={checkout.projectId} />
          <ProjectSubagentAuditControl key={`audit-${checkout.projectId}`} projectId={checkout.projectId} />
          <ProjectQuestionTimeout key={`timeout-${checkout.projectId}`} projectId={checkout.projectId} />
          <PolicyForm fields={policyFields.execution.filter(({ id }) => !["planning-model", "implementation-model", "subagent-policy", "subagent-cap", "subagent-audit", "live-wait"].includes(id))}
            stored={undefined} simulate={simulate} />
        </>;
      case "git":
        if (simulation) return <><SimulatedDeliveryGitSettings />{policy("git")}</>;
        if (!checkout) return policy("git");
        return <>
          <CandidateGateSettings key={checkout.projectId} projectId={checkout.projectId} />
          <DeliveryGitSettings key={`delivery-${checkout.projectId}`} projectId={checkout.projectId} />
          <PolicyForm fields={policyFields.git} stored={undefined} simulate={simulate} />
        </>;
      case "automation":
        if (simulation || !checkout) return policy("automation");
        return <>
          <PolicyForm fields={policyFields.automation.filter(({ id }) => id === "implementation-release")} stored={undefined} simulate={simulate} />
          <WorkflowSettingsProposals key={checkout.projectId} checkout={checkout} onOpenRun={(runId) => {
            onOpenChange(false);
            openActivity(runId);
          }} />
        </>;
      default:
        return policy(currentPage.id);
    }
  }

  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog.Portal>
        <Dialog.Overlay className="settings-backdrop" />
        <Dialog.Content
          className="settings-dialog"
          data-show-page={mobilePage}
          data-app-theme={simulation ? presentation.theme : undefined}
          data-app-motion={simulation ? presentation.motion : undefined}
          style={simulation ? presentation.style : undefined}
          onOpenAutoFocus={(event) => {
            previouslyFocused.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
            const target = !pageFromLink(initialPage) && searchInput.current?.getClientRects().length
              ? searchInput.current : pageTitle.current;
            if (target?.getClientRects().length) {
              event.preventDefault();
              target.focus();
            }
          }}
          onCloseAutoFocus={(event) => {
            if (previouslyFocused.current?.isConnected) {
              event.preventDefault();
              previouslyFocused.current.focus();
            }
          }}
        >
          <header className="settings-header">
            <span className="settings-heading-icon"><Settings2 size={17} aria-hidden="true" /></span>
            <Dialog.Title asChild><h1>Settings</h1></Dialog.Title>
            <Dialog.Description className="sr-only">Edit device, project and checkout preferences.</Dialog.Description>
            {simulation && <span className="settings-example-label">Simulation</span>}
            <Dialog.Close type="button" className="settings-icon-button" aria-label="Close settings">
              <X size={18} aria-hidden="true" />
            </Dialog.Close>
          </header>
          <div className="settings-layout">
            <aside className="settings-sidebar">
              <div className="settings-search">
                <Search size={16} aria-hidden="true" />
                <input ref={searchInput} type="search" aria-label="Search settings" placeholder="Search settings"
                  value={query} onChange={(event) => setQuery(event.target.value)} />
                {showResults && <button type="button" className="settings-clear-search" aria-label="Clear settings search"
                  onClick={() => { setQuery(""); searchInput.current?.focus(); }}>
                  <X size={14} aria-hidden="true" />
                </button>}
              </div>
              <nav className="settings-navigation" aria-label="Settings categories">
                {showResults ? (
                  <>
                    <h3>Search results</h3>
                    {results.map(({ page, entry }) => (
                      <button type="button" key={`${page.id}:${entry?.id ?? ""}`}
                        className="settings-search-result" aria-label={entry && `${entry.label} in ${page.title}`}
                        onClick={() => navigate(page.id, entry?.id)}>
                        <page.icon size={16} aria-hidden="true" />
                        <span>
                          <span>{entry?.label ?? page.title}</span>
                          {entry && <small>{page.title}</small>}
                        </span>
                        <ChevronRight size={14} aria-hidden="true" />
                      </button>
                    ))}
                    {results.length === 0 &&
                      <p role="status" className="settings-no-results">No settings match “{query}”.</p>}
                  </>
                ) : groups.map(({ scope, label }) => {
                  const groupPages = available.filter((page) => page.scope === scope);
                  if (!groupPages.length) return null;
                  return <section className="settings-nav-group" key={scope} aria-label={label}>
                    <h3>{label}</h3>
                    {groupPages.map((page) => (
                      <button key={page.id} type="button"
                        className="settings-nav-button" aria-current={page.id === currentPage.id ? "page" : undefined}
                        onClick={() => navigate(page.id)}>
                        <page.icon size={16} aria-hidden="true" />
                        <span>{page.title}</span>
                        <ChevronRight className="settings-mobile-chevron" size={14} aria-hidden="true" />
                      </button>
                    ))}
                  </section>;
                })}
              </nav>
            </aside>
            <section className="settings-content" ref={content} aria-labelledby="settings-page-title">
              <button type="button" className="settings-back" onClick={() => {
                setMobilePage(false);
                requestAnimationFrame(() => searchInput.current?.focus());
              }}>
                <ArrowLeft size={16} aria-hidden="true" /> Settings
              </button>
              <div className="settings-page-heading">
                <span className="settings-heading-icon"><currentPage.icon size={21} aria-hidden="true" /></span>
                <div>
                  <small title={target.path}>{target.label}</small>
                  <h2 id="settings-page-title" ref={pageTitle} tabIndex={-1}>{currentPage.title}</h2>
                </div>
              </div>
              <div className="settings-page-body">{pageContent()}</div>
            </section>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

function AppearanceControls({
  settings,
  update,
}: {
  settings: AppearanceSettings;
  update: <Key extends keyof AppearanceSettings>(key: Key, value: AppearanceSettings[Key]) => void;
}) {
  function option<Key extends keyof AppearanceSettings>(
    key: Key,
    choices: readonly (readonly [value: AppearanceSettings[Key], label: string])[],
  ) {
    const row = appearanceEntries[key];
    return <SettingRow {...row}>
      <select id={row.id} aria-describedby={`${row.id}-description`} value={settings[key]}
        onChange={(event) => update(key, event.target.value as AppearanceSettings[Key])}>
        {choices.map(([value, label]) => <option key={value} value={value}>{label}</option>)}
      </select>
    </SettingRow>;
  }

  return (
    <>
      <div className="settings-row" data-setting="theme">
        <div className="settings-row-label">
          <span id="appearance-theme-label">Theme</span>
          <p id="appearance-theme-description">{appearanceEntries.theme.description}</p>
        </div>
        <div className="settings-theme-options" role="group" aria-labelledby="appearance-theme-label" aria-describedby="appearance-theme-description">
          {([{ key: "system", label: "System", icon: Monitor }, { key: "light", label: "Light", icon: Sun }, { key: "dark", label: "Dark", icon: Moon }] as const)
            .map(({ key, label, icon: Icon }) => (
              <button key={key} type="button" aria-pressed={settings.theme === key} onClick={() => update("theme", key)}>
                <Icon size={15} aria-hidden="true" /> {label}
              </button>
            ))}
        </div>
      </div>
      {option("interfaceFont", [["geist", "Geist"], ["segoe_ui", "Segoe UI"]])}
      {option("responseFont", [["interface", "Match interface"], ["spectral", "Spectral"]])}
      {option("conversationTextSize", [["small", "Small · 13 px"], ["medium", "Medium · 14 px"], ["large", "Large · 16 px"]])}
      {option("transcriptWidth", [["narrow", "Narrow · 640 px"], ["standard", "Standard · 736 px"], ["wide", "Wide · 896 px"]])}
      {option("motion", [["system", "System"], ["reduced", "Reduced"]])}
      <blockquote className="settings-response-example">
        <span>Response preview</span>
        The review keeps the original patch and records the verified target.
      </blockquote>
    </>
  );
}

function CustomizationExample({ approved, approve }: { approved: boolean; approve: () => void }) {
  return <div className="settings-system" data-setting="customization-inventory">
    <section className="settings-system-group">
      <h3>Executable entries</h3>
      <details className="settings-matrix" open>
        <summary>review-check · Codex · {approved ? "Revision approved" : "Pending review"}</summary>
        <dl className="settings-facts">
          <dt>Source</dt><dd><code>.codex/config.toml</code></dd>
          <dt>Kind</dt><dd>MCP server</dd>
          <dt>Target</dt><dd><code>scripts/review-check.ps1</code></dd>
          <dt>Scope</dt><dd>Example checkout · read-only</dd>
          <dt>Revision</dt><dd><code>2a1f49c608bd</code></dd>
        </dl>
        <Button type="button" disabled={approved} onClick={approve}>{approved ? "Approved" : "Approve revision"}</Button>
      </details>
    </section>
    <section className="settings-system-group">
      <h3>Markdown instructions</h3>
      <details className="settings-matrix">
        <summary>AGENTS.md · revision 9b00ad2</summary>
        <p>Review the exact candidate diff and keep its verification evidence with the task.</p>
      </details>
    </section>
  </div>;
}
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
