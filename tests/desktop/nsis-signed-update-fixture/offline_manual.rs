use super::*;

impl VerifiedMaintenanceUpdate {
    pub async fn prepare_signed_desktop_fixture<R: Runtime>(
        app: &AppHandle<R>,
        store: &Store,
        request: MaintenanceActivationRequest,
        package_root: &Path,
        manifest: serde_json::Value,
    ) -> Result<Self> {
        ensure!(
            app.config().identifier == env!("OMNIAGENT_SIGNED_FIXTURE_ID"),
            "Wrong updater fixture product"
        );
        let trust =
            TrustedUpdateFeed::application()?.context("Missing ephemeral test updater key")?;
        ensure!(
            trust.endpoint.host_str() == Some("release.invalid"),
            "The updater fixture requires its offline endpoint"
        );
        let handoff = store.require_signed_update_handoff(request.clone()).await?;
        let update = trust
            .build(app)?
            .restore_update(manifest)?
            .context("Missing newer fixture release")?;
        let bytes = read_handoff_package(package_root, &handoff)?;
        verify_package(&update, &handoff, &bytes)?;
        ensure!(
            store.require_signed_update_handoff(request.clone()).await? == handoff,
            "The accepted handoff changed during offline verification"
        );
        Ok(Self {
            updater: update,
            request,
            handoff,
            bytes,
        })
    }
}
