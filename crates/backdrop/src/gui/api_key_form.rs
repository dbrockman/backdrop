//! The Wallhaven API key form, shown in a dialog from the Wallhaven page.
//!
//! A key is checked with Wallhaven before it is saved. One Wallhaven rejects
//! stays in the field with an explanation; one that can't be checked, for
//! example offline, is saved with a warning. A saved key is never shown again:
//! the field only takes a new one.
//!
//! The dialog owns Enter, so its `on_ok` calls [`ApiKeyForm::save`] and keeps
//! the dialog open; a successful save closes it.

use std::path::PathBuf;
use std::sync::Arc;

use backdrop_core::providers::{KeyCheck, Provider, check_api_key};
use backdrop_core::settings::Settings;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::link::Link;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, StyledExt as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// Where a Wallhaven account shows its API key.
const ACCOUNT_SETTINGS_URL: &str = "https://wallhaven.cc/settings/account";

/// Emitted when the stored key was saved or removed.
pub struct ApiKeyChanged;

/// How saving a key ended.
enum Outcome {
    /// Saved; with a message when the key couldn't be checked.
    Saved(Option<String>),
    Rejected,
    Failed(String),
}

pub struct ApiKeyForm {
    provider: Arc<dyn Provider>,
    settings_file: PathBuf,
    input: Entity<InputState>,
    /// A save or removal is in progress.
    saving: bool,
    error: Option<SharedString>,
    _input_subscription: Subscription,
}

impl ApiKeyForm {
    pub fn new(
        provider: Arc<dyn Provider>,
        settings_file: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("Paste your API key")
        });
        // A new key replaces the explanation of why the last one failed. Enter
        // reaches the dialog rather than the field, through `on_ok`.
        let input_subscription = cx.subscribe(&input, |this, _, event, cx| {
            if let InputEvent::Change = event
                && this.error.is_some()
            {
                this.error = None;
                cx.notify();
            }
        });
        Self {
            provider,
            settings_file,
            input,
            saving: false,
            error: None,
            _input_subscription: input_subscription,
        }
    }

    pub fn input(&self) -> &Entity<InputState> {
        &self.input
    }

    /// Checks the entered key, then saves it and closes the dialog.
    pub fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let key = self.input.read(cx).value().trim().to_owned();
        if key.is_empty() {
            self.error = Some("Paste the key from your Wallhaven account settings.".into());
            cx.notify();
            return;
        }
        self.saving = true;
        self.error = None;
        cx.notify();

        let provider = self.provider.clone();
        let settings_file = self.settings_file.clone();
        let saved_key = key.clone();
        cx.spawn_in(window, async move |this, cx| {
            let outcome = cx
                .background_spawn(async move {
                    let warning = match check_api_key(provider.as_ref(), &saved_key) {
                        Ok(KeyCheck::Accepted) => None,
                        Ok(KeyCheck::Unchecked(error)) => Some(error.to_string()),
                        Err(_) => return Outcome::Rejected,
                    };
                    let saved = Settings::update(&settings_file, |settings| {
                        settings.set_wallhaven_api_key(Some(&saved_key));
                        Ok(())
                    });
                    match saved {
                        Ok(_) => Outcome::Saved(warning),
                        Err(error) => Outcome::Failed(error.to_string()),
                    }
                })
                .await;
            this.update_in(cx, |form, window, cx| {
                form.saving = false;
                match outcome {
                    Outcome::Saved(warning) => {
                        form.provider.set_api_key(Some(key));
                        cx.emit(ApiKeyChanged);
                        window.close_dialog(cx);
                        if let Some(warning) = warning {
                            window.push_notification(
                                Notification::warning(warning)
                                    .title("Saved the API key without checking it"),
                                cx,
                            );
                        }
                    }
                    Outcome::Rejected => {
                        form.error = Some(
                            "Wallhaven didn’t accept this key. Copy it again from your \
                             account settings."
                                .into(),
                        );
                    }
                    Outcome::Failed(message) => {
                        form.error = Some(format!("Couldn’t save the key: {message}").into());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Removes the stored key and closes the dialog.
    pub fn remove(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        self.saving = true;
        cx.notify();
        let settings_file = self.settings_file.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    Settings::update(&settings_file, |settings| {
                        settings.set_wallhaven_api_key(None);
                        Ok(())
                    })
                })
                .await;
            this.update_in(cx, |form, window, cx| {
                form.saving = false;
                match result {
                    Ok(_) => {
                        form.provider.set_api_key(None);
                        cx.emit(ApiKeyChanged);
                        window.close_dialog(cx);
                    }
                    Err(error) => {
                        form.error = Some(format!("Couldn’t remove the key: {error}").into());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

impl EventEmitter<ApiKeyChanged> for ApiKeyForm {}

impl Render for ApiKeyForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let has_key = self.provider.has_api_key();
        let note = match (&self.error, has_key) {
            (Some(error), _) => Some(div().text_color(theme.danger).child(error.clone())),
            (None, true) => Some(
                div()
                    .text_color(theme.muted_foreground)
                    .child("A key is saved. Paste a new one to replace it."),
            ),
            (None, false) => None,
        };

        v_flex()
            .gap_4()
            .child(
                v_flex()
                    .gap_1()
                    .text_sm()
                    .child(div().text_color(theme.muted_foreground).child(
                        "NSFW wallpapers need the API key of your Wallhaven account. \
                             Backdrop keeps it in its settings on this computer, never in \
                             the library.",
                    ))
                    .child(
                        Link::new("wallhaven-account-settings")
                            .href(ACCOUNT_SETTINGS_URL)
                            .child("Wallhaven account settings"),
                    ),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_sm().font_medium().child("API key"))
                    .child(
                        Input::new(&self.input)
                            .id("api-key-input")
                            .mask_toggle()
                            .disabled(self.saving),
                    )
                    .when_some(note, |field, note| field.child(note.text_sm())),
            )
            .child(
                h_flex()
                    .gap_2()
                    .when(has_key, |footer| {
                        footer.child(
                            Button::new("remove-api-key")
                                .outline()
                                .label("Remove Key")
                                .disabled(self.saving)
                                .on_click(
                                    cx.listener(|form, _, window, cx| form.remove(window, cx)),
                                ),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        Button::new("cancel-api-key")
                            .outline()
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("save-api-key")
                            .primary()
                            .label("Save")
                            .loading(self.saving)
                            .on_click(cx.listener(|form, _, window, cx| form.save(window, cx))),
                    ),
            )
    }
}
