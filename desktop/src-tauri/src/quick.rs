//! Android quick-unlock (fingerprint / face / the phone's own PIN, pattern or password): the Kotlin side
//! (`QuickUnlockPlugin.kt`, in the app module) seals the wallet's password with an Android Keystore AES-256-GCM key that needs the
//! user's authentication for EVERY use (BiometricPrompt with a CryptoObject; strong biometrics, or the device credential on
//! Android 11+; hardware-backed, in StrongBox where there is one; the key is lost when biometrics are enrolled or removed). The
//! sealed blob is kept by the page next to the wallet; unsealing needs the prompt, so a copy of the app's files is useless without
//! the phone. This file is only the bridge: it carries the secret to Kotlin and back and keeps nothing.
use crate::{AppError, Res};
use serde::Serialize;
use serde_json::{Value, json};
use tauri::plugin::{Builder, PluginHandle, TauriPlugin};
use tauri::{AppHandle, Manager, Runtime};

/// The Kotlin plugin's handle (Android; on any other mobile OS there is none and quick-unlock reports itself unavailable).
pub struct Quick<R: Runtime>(Option<PluginHandle<R>>);

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("quickunlock")
        .setup(|app, _api| {
            #[cfg(target_os = "android")]
            let handle = Some(_api.register_android_plugin("io.github.gumbo64.sm64coin", "QuickUnlockPlugin")?);
            #[cfg(not(target_os = "android"))]
            let handle = None;
            app.manage(Quick::<R>(handle));
            Ok(())
        })
        .build()
}

fn code_of(c: Option<&str>) -> &'static str {
    match c {
        Some("cancelled") => "cancelled",
        Some("lockout") => "lockout",
        Some("invalidated") => "invalidated",
        Some("unavailable") => "unavailable",
        _ => "quick_failed",
    }
}

async fn run<R: Runtime>(app: &AppHandle<R>, command: &str, payload: impl Serialize) -> Res<Value> {
    let st = app.state::<Quick<R>>();
    let Some(h) = st.0.as_ref() else { return Err(AppError::new("unavailable", "quick unlock is not available on this device")) };
    h.run_mobile_plugin_async::<Value>(command, payload).await.map_err(|e| match e {
        tauri::plugin::mobile::PluginInvokeError::InvokeRejected(r) => AppError::new(code_of(r.code.as_deref()), r.message.unwrap_or_else(|| "quick unlock failed".into())),
        other => AppError::new("quick_failed", other.to_string()),
    })
}

/// `{available: bool, reason?}`: a screen lock and an enrolled fingerprint (or, on Android 11+, a device PIN/pattern/password).
#[tauri::command]
pub async fn quick_status(app: AppHandle) -> Res<Value> {
    match run(&app, "status", json!({})).await {
        Ok(v) => Ok(v),
        Err(e) if e.code == "unavailable" => Ok(json!({"available": false, "reason": "unsupported"})),
        Err(e) => Err(e),
    }
}

/// Seals `secret` (asks the user to authenticate): resolves `{blob}` (base64, safe to store). Replaces the Keystore key.
#[tauri::command]
pub async fn quick_wrap(app: AppHandle, secret: String) -> Res<Value> {
    let secret = zeroize::Zeroizing::new(secret);
    run(&app, "wrap", json!({"secret": secret.as_str()})).await
}

/// Unseals a blob made by `quick_wrap` (asks the user to authenticate): resolves `{secret}`.
#[tauri::command]
pub async fn quick_unwrap(app: AppHandle, blob: String) -> Res<Value> {
    run(&app, "unwrap", json!({"blob": blob})).await
}

/// Deletes the Keystore key (quick-unlock turned off, or the wallet forgotten).
#[tauri::command]
pub async fn quick_forget(app: AppHandle) -> Res<Value> {
    run(&app, "forget", json!({})).await
}
