use serde_json::json;
use tauri::{AppHandle, Manager};

use crate::core::Result;
use crate::settings::{Settings, SettingsStore};
use crate::window;

const MAX_ONBOARDING_STEP: u32 = 20;

#[tauri::command]
pub async fn open_onboarding(app: AppHandle) -> Result<()> {
    window::open_onboarding(&app)
}

#[tauri::command]
pub async fn set_onboarding_step(app: AppHandle, step: u32) -> Result<Settings> {
    update_onboarding_settings(
        &app,
        json!({
            "onboarding": {
                "lastStep": step.min(MAX_ONBOARDING_STEP),
            },
        }),
    )
}

#[tauri::command]
pub async fn finish_onboarding(app: AppHandle) -> Result<Settings> {
    let next = update_onboarding_settings(
        &app,
        json!({
            "onboarding": {
                "completed": true,
                "lastStep": 0,
            },
        }),
    )?;

    if let Err(err) = window::hide_window(&app, window::ONBOARDING_WINDOW_LABEL) {
        log::warn!("hide onboarding window after finish failed: {err}");
    }

    window::show_window(&app, window::CLIPBOARD_WINDOW_LABEL)?;

    Ok(next)
}

fn update_onboarding_settings(app: &AppHandle, patch: serde_json::Value) -> Result<Settings> {
    let next = app.state::<SettingsStore>().update(patch)?;

    super::settings::emit_settings_updated(app, &next);

    Ok(next)
}
