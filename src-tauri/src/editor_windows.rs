use tauri::{
    App, Manager, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder, webview::NewWindowResponse,
};

const POPOUT_PATH: &str = "/editor-popout.html";
const EDITOR_LABEL: &str = "editor-";

fn admits_editor_popup(opener: &Url, requested: &Url) -> bool {
    requested.scheme() == opener.scheme()
        && requested.host_str() == opener.host_str()
        && requested.port_or_known_default() == opener.port_or_known_default()
        && requested.username().is_empty()
        && requested.password().is_none()
        && requested.path() == POPOUT_PATH
        && requested.query().is_none()
        && requested.fragment().is_none()
}

pub fn build_main_window(app: &App) -> tauri::Result<()> {
    let Some(config) = app
        .config()
        .app
        .windows
        .iter()
        .find(|config| config.label == "main")
    else {
        return Ok(());
    };
    let handle = app.handle().clone();
    WebviewWindowBuilder::from_config(app, config)?
        .on_new_window(move |requested, features| {
            let Some(opener) = handle
                .get_webview_window("main")
                .and_then(|window| window.url().ok())
            else {
                return NewWindowResponse::Deny;
            };
            if !admits_editor_popup(&opener, &requested) {
                return NewWindowResponse::Deny;
            }
            let label = format!("{EDITOR_LABEL}{}", uuid::Uuid::new_v4());
            let blank = Url::parse("about:blank").expect("valid static URL");
            let initial_navigation = blank.clone();
            match WebviewWindowBuilder::new(&handle, &label, WebviewUrl::External(blank))
                .window_features(features)
                .title("OmniAgent Editor")
                .min_inner_size(360.0, 240.0)
                .initialization_script(close_editor_script(&label))
                .on_navigation(move |url| url == &requested || url == &initial_navigation)
                .on_new_window(|_, _| NewWindowResponse::Deny)
                .on_document_title_changed(|window, title| {
                    if !title.is_empty() {
                        let _ = window.set_title(&title);
                    }
                })
                .build()
            {
                Ok(window) => NewWindowResponse::Create { window },
                Err(error) => {
                    eprintln!("Could not open an editor window: {error}");
                    NewWindowResponse::Deny
                }
            }
        })
        .build()?;
    Ok(())
}

fn close_editor_script(label: &str) -> String {
    // WebView2's default window.close() destroys the view inside the native window.
    // Close the native window through the opener so Dockview can detect and redock it.
    format!(
        r#"(() => {{
            const closeView = window.close.bind(window);
            window.close = () => {{
                const opener = window.opener?.__TAURI_INTERNALS__;
                if (opener) {{
                    void opener.invoke("close_editor_window", {{ label: "{label}" }}).catch(closeView);
                }} else {{
                    closeView();
                }}
            }};
        }})()"#,
    )
}

#[tauri::command]
pub async fn close_editor_window(window: WebviewWindow, label: String) -> Result<(), String> {
    if window.label() != "main"
        || label
            .strip_prefix(EDITOR_LABEL)
            .and_then(|id| uuid::Uuid::parse_str(id).ok())
            .is_none()
    {
        return Err("Invalid editor window".into());
    }
    if let Some(popup) = window.get_webview_window(&label) {
        popup.close().map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_only_the_static_editor_page_on_the_exact_opener_origin() {
        for origin in ["http://tauri.localhost", "http://127.0.0.1:1420"] {
            let opener = Url::parse(&format!("{origin}/#workbench")).unwrap();
            let admitted = Url::parse(&format!("{origin}{POPOUT_PATH}")).unwrap();
            assert!(admits_editor_popup(&opener, &admitted));
            for url in [
                format!("{origin}{POPOUT_PATH}?checkout=other"),
                format!("{origin}{POPOUT_PATH}#other"),
                format!("{origin}/index.html"),
                format!("{origin}9{POPOUT_PATH}"),
                format!("https://remote.test{POPOUT_PATH}"),
            ] {
                assert!(
                    !admits_editor_popup(&opener, &Url::parse(&url).unwrap()),
                    "{url}"
                );
            }
        }
    }
}
