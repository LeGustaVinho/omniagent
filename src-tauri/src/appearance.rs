use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use tauri::State;
use ts_rs::TS;

use crate::store::Store;

const APPEARANCE_KEY: &str = "appearance_settings";

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/")]
pub enum AppearanceTheme {
    #[default]
    Dark,
    Light,
    System,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/")]
pub enum InterfaceFont {
    #[default]
    Geist,
    SegoeUi,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/")]
pub enum ResponseFont {
    #[default]
    Interface,
    Spectral,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/")]
pub enum ConversationTextSize {
    Small,
    #[default]
    Medium,
    Large,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/")]
pub enum TranscriptWidth {
    Narrow,
    #[default]
    Standard,
    Wide,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/")]
pub enum MarkdownWidth {
    Narrow,
    #[default]
    Standard,
    Wide,
    Full,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/")]
pub enum AppearanceMotion {
    #[default]
    System,
    Reduced,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize, TS)]
#[serde(default, rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct AppearanceSettings {
    pub theme: AppearanceTheme,
    pub interface_font: InterfaceFont,
    pub response_font: ResponseFont,
    pub conversation_text_size: ConversationTextSize,
    pub transcript_width: TranscriptWidth,
    pub markdown_width: MarkdownWidth,
    pub motion: AppearanceMotion,
}

impl Store {
    async fn appearance_settings(&self) -> anyhow::Result<AppearanceSettings> {
        self.execute(|db| {
            let record = db
                .query_row(
                    "SELECT value FROM workspace_settings WHERE key = ?1",
                    [APPEARANCE_KEY],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            Ok(match record {
                Some(record) => serde_json::from_str(&record)?,
                None => AppearanceSettings::default(),
            })
        })
        .await
    }

    async fn save_appearance_settings(
        &self,
        settings: AppearanceSettings,
    ) -> anyhow::Result<AppearanceSettings> {
        self.execute(move |db| {
            let value = serde_json::to_string(&settings)?;
            db.execute(
                "INSERT INTO workspace_settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                params![APPEARANCE_KEY, value],
            )?;
            Ok(settings)
        })
        .await
    }
}

#[tauri::command]
pub async fn appearance_settings(store: State<'_, Store>) -> Result<AppearanceSettings, String> {
    store
        .appearance_settings()
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn save_appearance_settings(
    store: State<'_, Store>,
    settings: AppearanceSettings,
) -> Result<AppearanceSettings, String> {
    store
        .save_appearance_settings(settings)
        .await
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_defaults_and_persists_appearance_across_reopen() -> anyhow::Result<()> {
        let folder = tempfile::tempdir()?;
        let path = folder.path().join("appearance.sqlite3");
        tauri::async_runtime::block_on(async {
            let store = Store::open(path.clone())?;
            assert_eq!(
                store.appearance_settings().await?,
                AppearanceSettings::default()
            );
            let chosen = AppearanceSettings {
                theme: AppearanceTheme::System,
                conversation_text_size: ConversationTextSize::Large,
                transcript_width: TranscriptWidth::Wide,
                markdown_width: MarkdownWidth::Full,
                ..AppearanceSettings::default()
            };
            assert_eq!(store.save_appearance_settings(chosen).await?, chosen);
            drop(store);
            let reopened = Store::open(path)?;
            assert_eq!(reopened.appearance_settings().await?, chosen);
            Ok(())
        })
    }
}
