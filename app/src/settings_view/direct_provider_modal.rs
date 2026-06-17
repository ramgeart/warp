use ai::providers::{DirectProvider, DirectProviderManager};
use warpui::elements::{
    ChildView, Container, CrossAxisAlignment, Flex, MainAxisAlignment, MainAxisSize, ParentElement,
    Text,
};
use warpui::ui_components::components::{UiComponent, UiComponentStyles};
use warpui::{
    AppContext, Element, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle,
};

use crate::appearance::{Appearance, AppearanceEvent};
use crate::editor::{
    EditorView, PropagateAndNoOpNavigationKeys, SingleLineEditorOptions, TextOptions,
};
use crate::view_components::action_button::{ActionButton, DangerSecondaryTheme, SecondaryTheme};

const LABEL_FONT_SIZE: f32 = 12.;
const INPUT_WIDTH: f32 = 480.;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectProviderModalEvent {
    Close,
    Save { provider: DirectProvider },
    Remove { provider_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectProviderModalAction {
    Cancel,
    Save,
    Remove,
}

pub struct DirectProviderModal {
    name_editor: ViewHandle<EditorView>,
    url_editor: ViewHandle<EditorView>,
    key_editor: ViewHandle<EditorView>,
    cancel_button: ViewHandle<ActionButton>,
    save_button: ViewHandle<ActionButton>,
    remove_button: ViewHandle<ActionButton>,
    /// Stable id of the provider being edited; `None` when adding a new one.
    editing_id: Option<String>,
}

impl DirectProviderModal {
    pub fn new(
        _modal_title: Option<String>,
        _idx: Option<usize>,
        ctx: &mut ViewContext<Self>,
    ) -> Self {
        // Refresh editor text colors whenever the theme changes.
        ctx.subscribe_to_model(&Appearance::handle(ctx), |me, _, event, ctx| {
            if let AppearanceEvent::ThemeChanged = event {
                me.update_editor_text_colors(ctx);
            }
        });

        let font_family = Appearance::as_ref(ctx).ui_font_family();
        let text_colors = crate::settings_view::editor_text_colors(Appearance::as_ref(ctx));

        let name_colors = text_colors.clone();
        let name_editor = ctx.add_typed_action_view(move |ctx| {
            let options = SingleLineEditorOptions {
                text: TextOptions {
                    font_family_override: Some(font_family),
                    text_colors_override: Some(name_colors.clone()),
                    ..Default::default()
                },
                propagate_and_no_op_vertical_navigation_keys:
                    PropagateAndNoOpNavigationKeys::Always,
                ..Default::default()
            };
            let mut editor = EditorView::single_line(options, ctx);
            editor.set_placeholder_text("e.g., ollama", ctx);
            editor
        });

        let url_colors = text_colors.clone();
        let url_editor = ctx.add_typed_action_view(move |ctx| {
            let options = SingleLineEditorOptions {
                text: TextOptions {
                    font_family_override: Some(font_family),
                    text_colors_override: Some(url_colors.clone()),
                    ..Default::default()
                },
                propagate_and_no_op_vertical_navigation_keys:
                    PropagateAndNoOpNavigationKeys::Always,
                ..Default::default()
            };
            let mut editor = EditorView::single_line(options, ctx);
            editor.set_placeholder_text("e.g., http://localhost:11434", ctx);
            editor
        });

        let key_colors = text_colors.clone();
        let key_editor = ctx.add_typed_action_view(move |ctx| {
            let options = SingleLineEditorOptions {
                is_password: true,
                text: TextOptions {
                    font_family_override: Some(font_family),
                    text_colors_override: Some(key_colors.clone()),
                    ..Default::default()
                },
                propagate_and_no_op_vertical_navigation_keys:
                    PropagateAndNoOpNavigationKeys::Always,
                ..Default::default()
            };
            let mut editor = EditorView::single_line(options, ctx);
            editor.set_placeholder_text("Leave blank if not required", ctx);
            editor
        });

        let cancel_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Cancel", SecondaryTheme).on_click(|ctx| {
                ctx.dispatch_typed_action(DirectProviderModalAction::Cancel);
            })
        });
        let save_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Save", SecondaryTheme).on_click(|ctx| {
                ctx.dispatch_typed_action(DirectProviderModalAction::Save);
            })
        });
        let remove_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Remove", DangerSecondaryTheme).on_click(|ctx| {
                ctx.dispatch_typed_action(DirectProviderModalAction::Remove);
            })
        });

        Self {
            name_editor,
            url_editor,
            key_editor,
            cancel_button,
            save_button,
            remove_button,
            editing_id: None,
        }
    }

    fn update_editor_text_colors(&mut self, ctx: &mut ViewContext<Self>) {
        let colors = crate::settings_view::editor_text_colors(Appearance::as_ref(ctx));
        for editor in [&self.name_editor, &self.url_editor, &self.key_editor] {
            editor.update(ctx, |editor, ctx| {
                editor.set_text_colors(colors.clone(), ctx);
            });
        }
    }

    pub fn on_open(&mut self, ctx: &mut ViewContext<Self>) {
        ctx.focus(&self.name_editor);
        ctx.notify();
    }

    pub fn on_close(&mut self, _ctx: &mut ViewContext<Self>) {}

    /// Prepare the editors for a new provider (clear everything).
    pub fn prefill_new(&mut self, ctx: &mut ViewContext<Self>) {
        self.editing_id = None;
        self.name_editor
            .update(ctx, |ed, ctx| ed.set_buffer_text("", ctx));
        self.url_editor
            .update(ctx, |ed, ctx| ed.set_buffer_text("", ctx));
        self.key_editor
            .update(ctx, |ed, ctx| ed.set_buffer_text("", ctx));
        ctx.notify();
    }

    /// Prepare the editors for editing an existing provider.
    pub fn prefill_edit(&mut self, provider: &DirectProvider, ctx: &mut ViewContext<Self>) {
        self.editing_id = Some(provider.id.clone());
        let name = provider.name.clone();
        let url = provider.base_url.clone();
        let key = provider.api_key.clone();
        self.name_editor
            .update(ctx, |ed, ctx| ed.set_buffer_text(&name, ctx));
        self.url_editor
            .update(ctx, |ed, ctx| ed.set_buffer_text(&url, ctx));
        self.key_editor
            .update(ctx, |ed, ctx| ed.set_buffer_text(&key, ctx));
        ctx.notify();
    }

    fn read_provider(&self, app: &AppContext) -> DirectProvider {
        let name = self.name_editor.as_ref(app).buffer_text(app).trim().to_string();
        let base_url = self.url_editor.as_ref(app).buffer_text(app).trim().to_string();
        let api_key = self.key_editor.as_ref(app).buffer_text(app).trim().to_string();

        if let Some(ref id) = self.editing_id {
            if let Some(existing) = DirectProviderManager::as_ref(app)
                .providers()
                .iter()
                .find(|p| &p.id == id)
                .cloned()
            {
                return DirectProvider {
                    name,
                    base_url,
                    api_key,
                    ..existing
                };
            }
        }

        DirectProvider {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            base_url,
            api_key,
            default_headers: Vec::new(),
            models: Vec::new(),
        }
    }
}

impl Entity for DirectProviderModal {
    type Event = DirectProviderModalEvent;
}

impl View for DirectProviderModal {
    fn ui_name() -> &'static str {
        "DirectProviderModal"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();

        let label_font_family = appearance.ui_font_family();
        let label_text_color = theme.active_ui_text_color().into();
        let label = move |text: &'static str| {
            Text::new(text, label_font_family, LABEL_FONT_SIZE)
                .with_color(label_text_color)
                .finish()
        };

        let input_style = UiComponentStyles {
            width: Some(INPUT_WIDTH),
            ..Default::default()
        };

        let mut column = Flex::column();

        // Description
        column.add_child(
            Container::new(
                Text::new(
                    "Call an OpenAI-compatible endpoint directly. Models are fetched from GET {base_url}/v1/models after saving and shown as {name}/{model}.",
                    appearance.ui_font_family(),
                    LABEL_FONT_SIZE,
                )
                .with_color(theme.nonactive_ui_text_color().into())
                .soft_wrap(true)
                .finish(),
            )
            .with_margin_bottom(16.)
            .finish(),
        );

        // Provider name
        column.add_child(
            Container::new(label("Provider name"))
                .with_margin_bottom(4.)
                .finish(),
        );
        column.add_child(
            Container::new(
                appearance
                    .ui_builder()
                    .text_input(self.name_editor.clone())
                    .with_style(input_style.clone())
                    .build()
                    .finish(),
            )
            .with_margin_bottom(16.)
            .finish(),
        );

        // Base URL
        column.add_child(
            Container::new(label("Base URL"))
                .with_margin_bottom(4.)
                .finish(),
        );
        column.add_child(
            Container::new(
                appearance
                    .ui_builder()
                    .text_input(self.url_editor.clone())
                    .with_style(input_style.clone())
                    .build()
                    .finish(),
            )
            .with_margin_bottom(16.)
            .finish(),
        );

        // API key
        column.add_child(
            Container::new(label("API key (optional)"))
                .with_margin_bottom(4.)
                .finish(),
        );
        column.add_child(
            Container::new(
                appearance
                    .ui_builder()
                    .text_input(self.key_editor.clone())
                    .with_style(input_style)
                    .build()
                    .finish(),
            )
            .with_margin_bottom(16.)
            .finish(),
        );

        // Buttons row
        let mut buttons_row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_main_axis_alignment(MainAxisAlignment::End)
            .with_cross_axis_alignment(CrossAxisAlignment::Center);

        if self.editing_id.is_some() {
            buttons_row.add_child(
                Container::new(ChildView::new(&self.remove_button).finish())
                    .with_margin_right(8.)
                    .finish(),
            );
        }
        buttons_row.add_child(
            Container::new(ChildView::new(&self.cancel_button).finish())
                .with_margin_right(8.)
                .finish(),
        );
        buttons_row.add_child(ChildView::new(&self.save_button).finish());

        column.add_child(buttons_row.finish());

        column.finish()
    }
}

impl TypedActionView for DirectProviderModal {
    type Action = DirectProviderModalAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            DirectProviderModalAction::Cancel => {
                ctx.emit(DirectProviderModalEvent::Close);
            }
            DirectProviderModalAction::Save => {
                let provider = self.read_provider(ctx);
                ctx.emit(DirectProviderModalEvent::Save { provider });
            }
            DirectProviderModalAction::Remove => {
                if let Some(id) = self.editing_id.clone() {
                    ctx.emit(DirectProviderModalEvent::Remove { provider_id: id });
                }
            }
        }
    }
}
