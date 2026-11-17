use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use minisign_verify::{PublicKey, Signature};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Runtime, Url};
use tauri_plugin_updater::{Update, Updater, UpdaterBuilder, UpdaterExt};

use crate::{
    providers::configuration::open_locked_local_file,
    store::Store,
    workflows::maintenance::{MaintenanceActivationRequest, MaintenanceHandoff},
};

const MAX_INSTALLER_BYTES: u64 = 512 * 1024 * 1024;
const FEED_TIMEOUT: Duration = Duration::from_secs(30);

mod activation;
mod journal;
mod recovery;
pub use journal::{ActivationOutcome, UpdateActivationReceipt, UpdateBackup, UpdateJournal};
pub use recovery::{PreviousMaintenanceUpdate, restore_previous_for_startup};

pub struct TrustedUpdateFeed {
    endpoint: Url,
    public_key: String,
}

impl TrustedUpdateFeed {
    pub fn application() -> Result<Option<Self>> {
        match (
            option_env!("OMNIAGENT_UPDATE_FEED"),
            option_env!("OMNIAGENT_UPDATE_PUBLIC_KEY"),
        ) {
            (None, None) => Ok(None),
            (Some(endpoint), Some(key)) => Ok(Some(Self::new(endpoint, key)?)),
            _ => anyhow::bail!(
                "This build must configure both the signed update feed and its public key"
            ),
        }
    }

    fn new(endpoint: &str, public_key: &str) -> Result<Self> {
        let endpoint: Url = endpoint
            .parse()
            .context("The signed update feed URL is invalid")?;
        require_https(&endpoint)?;
        let public_key = public_key.trim();
        let decoded =
            decode_text(public_key).context("The configured updater public key is invalid")?;
        PublicKey::decode(&decoded)
            .context("The configured updater public key is not a Minisign public key")?;
        Ok(Self {
            endpoint,
            public_key: public_key.into(),
        })
    }

    fn build<R: Runtime>(&self, app: &AppHandle<R>) -> Result<Updater> {
        Ok(self.builder(app)?.build()?)
    }

    fn builder<R: Runtime>(&self, app: &AppHandle<R>) -> Result<UpdaterBuilder> {
        Ok(app
            .updater_builder()
            .endpoints(vec![self.endpoint.clone()])?
            .pubkey(self.public_key.clone())
            .timeout(FEED_TIMEOUT)
            .configure_client(|client| client.https_only(true)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignedUpdatePreview {
    pub handoff_id: String,
    pub current_version: String,
    pub version: String,
    pub source_revision: String,
    pub sha256: String,
    pub size: u64,
    pub verification_id: String,
    pub checks: Vec<String>,
    pub approval_artifact_id: String,
}

pub struct VerifiedMaintenanceUpdate {
    handoff: MaintenanceHandoff,
    request: MaintenanceActivationRequest,
    updater: Update,
    bytes: Vec<u8>,
}

impl VerifiedMaintenanceUpdate {
    pub fn preview(&self) -> SignedUpdatePreview {
        let handoff = &self.handoff;
        SignedUpdatePreview {
            handoff_id: handoff.id.clone(),
            current_version: self.updater.current_version.clone(),
            version: self.updater.version.clone(),
            source_revision: handoff.source_revision.clone(),
            sha256: handoff.sha256.clone(),
            size: handoff.size,
            verification_id: handoff.verification_id.clone(),
            checks: handoff.checks.clone(),
            approval_artifact_id: handoff.approval_artifact_id.clone(),
        }
    }

    pub async fn prepare<R: Runtime>(
        app: &AppHandle<R>,
        store: &Store,
        trust: &TrustedUpdateFeed,
        request: MaintenanceActivationRequest,
        package_root: PathBuf,
    ) -> Result<Self> {
        let handoff = store.require_signed_update_handoff(request.clone()).await?;
        let updater = trust.build(app)?;
        let advertised = updater
            .check()
            .await
            .context("Cannot check the configured signed update feed")?
            .context("The trusted feed reports no newer version")?;

        // Reparse the selected feed response with Tauri's stricter cached-update
        // verifier so the Minisign signature must authenticate the version.
        let update = updater
            .restore_update(advertised.raw_json)?
            .context("The trusted feed no longer describes a newer version")?;
        let (bytes, handoff, update) = tokio::task::spawn_blocking(move || {
            let bytes = read_handoff_package(&package_root, &handoff)?;
            verify_package(&update, &handoff, &bytes)?;
            Ok::<_, anyhow::Error>((bytes, handoff, update))
        })
        .await??;
        ensure!(
            store.require_signed_update_handoff(request.clone()).await? == handoff,
            "The accepted maintenance handoff changed during verification"
        );
        Ok(Self {
            handoff,
            request,
            updater: update,
            bytes,
        })
    }
}

fn verify_package(update: &Update, handoff: &MaintenanceHandoff, bytes: &[u8]) -> Result<()> {
    verify_payload(
        update,
        &handoff.version,
        &handoff.source_revision,
        handoff.size,
        &handoff.sha256,
        bytes,
    )
}

fn verify_payload(
    update: &Update,
    version: &str,
    source_revision: &str,
    size: u64,
    sha256: &str,
    bytes: &[u8],
) -> Result<()> {
    require_https(&update.download_url)?;
    ensure!(
        update.version == version.strip_prefix('v').unwrap_or(version),
        "The signed feed version differs from the accepted maintenance package version"
    );
    ensure!(
        bytes.len() as u64 == size && format!("{:x}", Sha256::digest(bytes)) == sha256,
        "The installer bytes differ from the exact accepted maintenance package"
    );
    update
        .verify(bytes)
        .context("Tauri rejected the installer signature or its signed version")?;

    // Tauri has checked Minisign's global signature, including this trusted comment.
    // Reject duplicate fields so all consumers agree on the signed provenance.
    let signature = Signature::decode(&decode_text(&update.signature)?)?;
    let comment = signature.trusted_comment();
    let source = single_signed_field(comment, "source:")?;
    ensure!(
        matches!(source.len(), 40 | 64)
            && source.bytes().all(|byte| byte.is_ascii_hexdigit())
            && source == source_revision,
        "The installer signature names a different source commit"
    );
    let version = single_signed_field(comment, "version:")?;
    ensure!(
        version.strip_prefix('v').unwrap_or(version) == update.version,
        "The installer signature names a different version"
    );
    Ok(())
}

fn read_handoff_package(root: &Path, handoff: &MaintenanceHandoff) -> Result<Vec<u8>> {
    let root =
        dunce::canonicalize(root).context("The maintenance package directory is unavailable")?;
    let id = uuid::Uuid::parse_str(&handoff.id).context("The maintenance handoff ID is invalid")?;
    let file = open_locked_local_file(&root.join(format!("{id}.package")))
        .context("The accepted maintenance installer is unavailable")?;
    read_package_bytes(file, handoff.size)
}

fn read_package_bytes(file: File, size: u64) -> Result<Vec<u8>> {
    ensure!(
        size > 0 && size <= MAX_INSTALLER_BYTES,
        "The signed installer has an invalid package size"
    );
    ensure!(
        file.metadata()?.len() == size,
        "The saved maintenance package has a different size from its receipt"
    );
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(usize::try_from(size)?)?;
    file.take(size + 1).read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn require_https(url: &Url) -> Result<()> {
    ensure!(
        url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none(),
        "The signed updater requires an HTTPS URL without credentials or a fragment"
    );
    Ok(())
}

fn decode_text(encoded: &str) -> Result<String> {
    Ok(String::from_utf8(STANDARD.decode(encoded)?)?)
}

fn single_signed_field<'a>(comment: &'a str, prefix: &str) -> Result<&'a str> {
    let mut matches = comment
        .split('\t')
        .filter_map(|field| field.strip_prefix(prefix));
    let value = matches
        .next()
        .context("The installer signature is missing signed provenance")?;
    ensure!(
        !value.is_empty() && matches.next().is_none(),
        "The installer signature has ambiguous signed provenance"
    );
    Ok(value)
}

#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) use tests::exercise_signed_handoff_activation;
