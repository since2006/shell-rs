//! The menu that picks a model, in the panel and in the settings: each
//! model service's models under the service's name.

use gpui_kit::component::menu::PopupMenu;
use gpui_kit::{Action, SharedString};

use crate::app::AiModelChoice;
use crate::host::AiProvider;

/// The model new conversations use: the one the settings name, else the
/// first service's first.
pub fn default_model<'a>(
    providers: &'a [AiProvider],
    provider: Option<&str>,
    model: Option<&str>,
) -> Option<(&'a AiProvider, SharedString)> {
    let named = provider.and_then(|id| {
        providers
            .iter()
            .find(|provider| provider.public_id.as_str() == id)
    });
    match named {
        Some(provider) => Some((provider, provider.model_or_first(model)?)),
        None => {
            let first = providers.first()?;
            Some((first, first.model_or_first(None)?))
        }
    }
}

/// Each service's name and its models, in list order.
pub fn model_groups(providers: &[AiProvider]) -> Vec<(SharedString, Vec<AiModelChoice>)> {
    providers
        .iter()
        .map(|provider| {
            let models = provider
                .models
                .iter()
                .map(|model| AiModelChoice {
                    provider: provider.public_id.to_string(),
                    model: model.to_string(),
                })
                .collect();
            (provider.name.clone(), models)
        })
        .collect()
}

/// `groups` as items of `menu`, `chosen` checked, each sending what
/// `action` makes of it.
pub fn model_items(
    mut menu: PopupMenu,
    groups: &[(SharedString, Vec<AiModelChoice>)],
    chosen: Option<&AiModelChoice>,
    action: fn(AiModelChoice) -> Box<dyn Action>,
) -> PopupMenu {
    for (ix, (name, models)) in groups.iter().enumerate() {
        if ix > 0 {
            menu = menu.separator();
        }
        menu = menu.label(name.clone());
        for choice in models {
            menu = menu.menu_with_check(
                choice.model.clone(),
                chosen == Some(choice),
                action(choice.clone()),
            );
        }
    }
    menu
}
