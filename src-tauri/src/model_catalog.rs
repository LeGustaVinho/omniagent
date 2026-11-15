use std::sync::LazyLock;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::providers::Provider;

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct ModelRating {
    pub intelligence_tenths: u16,
    pub api_reference_cents: u16,
    pub price_score: u8,
}

#[derive(Debug, Deserialize)]
struct ModelKey {
    provider: Provider,
    id: String,
}

#[derive(Debug, Deserialize)]
pub struct RatedModel {
    #[serde(flatten)]
    canonical: ModelKey,
    pub name: String,
    #[serde(flatten)]
    pub rating: ModelRating,
    #[serde(default)]
    aliases: Vec<ModelKey>,
}

#[derive(Debug, Deserialize)]
pub struct ModelCatalog {
    pub version: u16,
    models: Vec<RatedModel>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ObservedModel {
    pub id: String,
    pub canonical_id: Option<String>,
    pub name: String,
    pub selectable: bool,
    pub unavailable_reason: Option<String>,
}

#[derive(Debug)]
pub struct RatedOption<'a> {
    pub model: &'a ObservedModel,
    pub reference: Option<&'a RatedModel>,
}

#[derive(Debug, Clone, Deserialize, Eq, Hash, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct ModelSelection {
    pub provider: Provider,
    pub profile_id: String,
    pub model_id: String,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct ModelOption {
    pub selection: ModelSelection,
    pub name: String,
    pub selectable: bool,
    pub unavailable_reason: Option<String>,
    pub rating: Option<ModelRating>,
    pub catalog_version: u16,
}

pub static MODEL_CATALOG: LazyLock<ModelCatalog> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../model-catalog/v1.json"))
        .expect("valid bundled model catalog")
});

impl ModelCatalog {
    pub fn lookup(&self, provider: Provider, id: &str) -> Option<&RatedModel> {
        // Match the live provider identity exactly. A display name or a floating
        // alias like "opus" may refer to different versions between profiles.
        self.models.iter().find(|model| {
            std::iter::once(&model.canonical)
                .chain(&model.aliases)
                .any(|key| key.provider == provider && key.id == id)
        })
    }

    pub fn join<'a>(
        &'a self,
        provider: Provider,
        inventory: &'a [ObservedModel],
    ) -> Vec<RatedOption<'a>> {
        inventory
            .iter()
            .map(|model| {
                let id = model.canonical_id.as_deref().unwrap_or(&model.id);
                RatedOption {
                    model,
                    reference: self.lookup(provider, id),
                }
            })
            .collect()
    }

    pub fn project_options(
        &self,
        provider: Provider,
        profile_id: &str,
        inventory: &[ObservedModel],
    ) -> Vec<ModelOption> {
        self.join(provider, inventory)
            .into_iter()
            .map(|option| ModelOption {
                selection: ModelSelection {
                    provider,
                    profile_id: profile_id.to_owned(),
                    model_id: option.model.id.clone(),
                },
                name: option.model.name.clone(),
                selectable: option.model.selectable,
                unavailable_reason: option.model.unavailable_reason.clone(),
                rating: option.reference.map(|reference| reference.rating.clone()),
                catalog_version: self.version,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn bundled_catalog_contains_unique_provider_scoped_ids() {
        assert_eq!(MODEL_CATALOG.version, 1);
        assert_eq!(MODEL_CATALOG.models.len(), 14);
        let mut seen = HashSet::new();
        for model in &MODEL_CATALOG.models {
            assert!(model.rating.intelligence_tenths <= 1000);
            assert!(model.rating.price_score <= 100);
            for key in std::iter::once(&model.canonical).chain(&model.aliases) {
                assert!(seen.insert((key.provider, key.id.as_str())));
            }
        }
        assert_eq!(
            MODEL_CATALOG
                .lookup(Provider::Antigravity, "gemini-4-argon-high")
                .unwrap()
                .name,
            "Gemini 4 Argon"
        );
        assert!(
            MODEL_CATALOG
                .lookup(Provider::Codex, "gemini-4-argon-high")
                .is_none()
        );
        assert!(MODEL_CATALOG.lookup(Provider::Claude, "opus").is_none());
    }

    #[test]
    fn rates_only_models_in_the_inventory_and_keeps_their_ids_and_eligibility() {
        let inventory = [
            ObservedModel {
                id: "gemini-4-argon-high".into(),
                canonical_id: None,
                name: "Gemini 4 Argon (High)".into(),
                selectable: false,
                unavailable_reason: Some("Not enabled in this profile".into()),
            },
            ObservedModel {
                id: "gemini-pro-orchestrator-g1-pro".into(),
                canonical_id: None,
                name: "Orchestrator (Argon)".into(),
                selectable: true,
                unavailable_reason: None,
            },
        ];
        let options = MODEL_CATALOG.join(Provider::Antigravity, &inventory);
        assert_eq!(options.len(), 2);
        assert_eq!(options[0].model.id, inventory[0].id);
        assert!(!options[0].model.selectable);
        assert_eq!(options[0].reference.unwrap().name, "Gemini 4 Argon");
        assert!(options[1].reference.is_none());
        let projected =
            MODEL_CATALOG.project_options(Provider::Antigravity, "workspace-profile", &inventory);
        assert_eq!(projected[0].selection.model_id, "gemini-4-argon-high");
        assert_eq!(projected[0].selection.profile_id, "workspace-profile");
        assert_eq!(projected[0].rating.as_ref().unwrap().price_score, 65);
        assert!(!projected[0].selectable);
        assert_eq!(projected[1].rating, None);
    }
}
