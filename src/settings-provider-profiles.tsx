import { ProviderSettings } from "@/provider-settings";
import { ModelCatalogSettings } from "@/model-catalog-settings";
import { QualificationMatrix } from "@/qualification-matrix";
import { IntegrationNotice, SettingRow } from "@/settings-controls";

export function ProviderProfiles({ checkoutId }: { checkoutId?: string }) {
  return (
    <>
      <ProviderSettings checkoutId={checkoutId} />
      <section className="settings-system-group">
        <ModelCatalogSettings />
      </section>
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
        <IntegrationNotice>
          Linked Hyper-V guests will appear here when available.
        </IntegrationNotice>
      </section>
    </>
  );
}
